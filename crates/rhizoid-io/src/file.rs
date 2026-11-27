//! Reading: [`File`], [`Directory`], [`KeyInfo`].

use core::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use rhizoid_source::{FileOffset, ReadAt};

use crate::compress;
use crate::core::buffer::RBuffer;
use crate::core::directory::{DirectoryRecord, FreeSegment, decode_free_list, decode_key_list};
use crate::core::error::{
    Context, Diagnostic, DiagnosticKind, Error, ErrorKind, Result, ResultExt,
};
use crate::core::header::FileHeader;
use crate::core::key::KeyHeader;
use crate::core::walk::{Step, walk};
use crate::ntuple::RNTuple;
use crate::streamer::{self, Decoder, DynObject, FromDyn, OnMismatch, StreamerInfo, StreamerSet};
use crate::tree::Tree;

/// How tolerant reading is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Strictness {
    /// Tolerate what ROOT tolerates (unclosed files, …) and report a
    /// [`Diagnostic`]. The default.
    #[default]
    Lenient,
    /// Turn every tolerated irregularity into an error.
    Strict,
}

/// Bounds on what a file may make the reader allocate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ReadLimits {
    max_object_bytes: u64,
}

impl ReadLimits {
    /// Tight limits for untrusted input (64 MiB per object).
    pub const fn untrusted() -> Self {
        Self {
            max_object_bytes: 64 << 20,
        }
    }

    /// Sets the largest decompressed object accepted. The default is 1 GiB.
    #[must_use]
    pub const fn max_object_bytes(mut self, bytes: u64) -> Self {
        self.max_object_bytes = bytes;
        self
    }
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_object_bytes: 1 << 30,
        }
    }
}

/// Options for opening a file.
#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    strictness: Strictness,
    limits: ReadLimits,
}

impl ReadOptions {
    /// Sets the strictness. The default is [`Strictness::Lenient`].
    #[must_use]
    pub fn strictness(mut self, s: Strictness) -> Self {
        self.strictness = s;
        self
    }

    /// Sets the allocation limits. The default is [`ReadLimits::default`].
    #[must_use]
    pub fn limits(mut self, l: ReadLimits) -> Self {
        self.limits = l;
        self
    }
}

/// An open ROOT file. Cheap to clone; `Send + Sync`; reads take `&self`.
#[derive(Clone)]
pub struct File {
    inner: Arc<Inner>,
    /// Outside `inner`, so a handle can be pointed at another source of the
    /// same bytes ([`File::with_source`]).
    source: Arc<dyn ReadAt>,
}

struct Inner {
    len: u64,
    header: FileHeader,
    name: String,
    title: String,
    root: Arc<Vec<KeyHeader>>,
    options: ReadOptions,
    diagnostics: Mutex<Vec<Diagnostic>>,
    streamers: OnceLock<Arc<StreamerSet>>,
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File")
            .field("name", &self.inner.name)
            .field("len", &self.inner.len)
            .field("version", &self.inner.header.version())
            .field("keys", &self.inner.root.len())
            .finish_non_exhaustive()
    }
}

impl File {
    /// Opens a local file with default options.
    ///
    /// # Errors
    ///
    /// I/O errors, `NotRootFile`, or `Malformed` if the header or root
    /// directory cannot be decoded. Unclosed or truncated files are recovered
    /// (see [`File::diagnostics`]) unless [`Strictness::Strict`] is set.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, ReadOptions::default())
    }

    /// Opens a local file with options.
    ///
    /// # Errors
    ///
    /// As [`File::open`].
    pub fn open_with(path: impl AsRef<Path>, options: ReadOptions) -> Result<Self> {
        fn inner(path: &Path, options: ReadOptions) -> Result<File> {
            let f = std::fs::File::open(path)?;
            File::from_source(f, options)
        }
        inner(path.as_ref(), options)
    }

    /// Opens a file served over HTTP(S), with coalesced range requests and
    /// a metadata cache ([`HttpSource::open`](crate::io::HttpSource::open)).
    ///
    /// # Errors
    ///
    /// As [`File::open`], plus network errors.
    #[cfg(all(feature = "http", not(target_arch = "wasm32")))]
    pub fn open_url(url: &str) -> Result<Self> {
        Self::open_url_with(url, ReadOptions::default())
    }

    /// [`File::open_url`] with options.
    ///
    /// # Errors
    ///
    /// As [`File::open_url`].
    #[cfg(all(feature = "http", not(target_arch = "wasm32")))]
    pub fn open_url_with(url: &str, options: ReadOptions) -> Result<Self> {
        Self::from_source(crate::io::HttpSource::open(url)?, options)
    }

    /// Opens any [`ReadAt`] source.
    ///
    /// # Errors
    ///
    /// As [`File::open`].
    pub fn from_source(source: impl ReadAt + 'static, options: ReadOptions) -> Result<Self> {
        Self::from_arc(Arc::new(source), options)
    }

    pub(crate) fn from_arc(source: Arc<dyn ReadAt>, options: ReadOptions) -> Result<Self> {
        let len = source.len()?;
        let probe =
            source.read_at(FileOffset::new(0)..FileOffset::new(len.min(FileHeader::PROBE_LEN)))?;
        let header = FileHeader::decode(&probe)?;

        // The TFile key at fBEGIN: key header, then TNamed (name, title), then
        // the root directory record.
        let begin = header.begin();
        let file_rec_len = u64::from(header.nbytes_name) + 60;
        let rec = source.read_at(begin..begin.checked_add(file_rec_len).unwrap_or(begin))?;
        let mut r = RBuffer::new(&rec, 0).at(begin);
        KeyHeader::decode(&mut r).context(Context::Offset(begin))?;
        let name = r.read_string()?;
        let title = r.read_string()?;
        let dir_at = usize::try_from(header.nbytes_name)
            .map_err(|_| Error::malformed("fNbytesName too large"))?;
        let dir_bytes = rec
            .get(dir_at..)
            .ok_or_else(|| Error::malformed("root directory record is truncated"))?;
        let root_dir = DirectoryRecord::decode(
            &mut RBuffer::new(dir_bytes, 0)
                .at(FileOffset::new(begin.get() + u64::from(header.nbytes_name))),
        )?;

        let mut diagnostics = Vec::new();
        let keys = match read_keys(&*source, &root_dir, len, header.end) {
            Ok(keys) => keys,
            Err(e) => {
                if options.strictness == Strictness::Strict {
                    return Err(Error::new(
                        ErrorKind::Strict,
                        format!("the key list is unreadable and recovery is disabled: {e}"),
                    ));
                }
                let keys = recover(&*source, &header, &root_dir, len);
                diagnostics.push(Diagnostic::new(
                    DiagnosticKind::Recovered,
                    format!("file was not closed or is truncated ({e}); recovered {} keys by walking the records", keys.len()),
                    None,
                ));
                keys
            }
        };

        Ok(Self {
            source,
            inner: Arc::new(Inner {
                len,
                header,
                name,
                title,
                root: Arc::new(keys),
                options,
                diagnostics: Mutex::new(diagnostics),
                streamers: OnceLock::new(),
            }),
        })
    }

    /// The file header.
    pub fn header(&self) -> &FileHeader {
        &self.inner.header
    }

    /// The file's name, as stored in the file.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The file's title.
    pub fn title(&self) -> &str {
        &self.inner.title
    }

    /// The size of the file in bytes.
    pub fn len(&self) -> u64 {
        self.inner.len
    }

    /// Whether the file has no bytes (never true for a valid file).
    pub fn is_empty(&self) -> bool {
        self.inner.len == 0
    }

    /// The root directory.
    pub fn root(&self) -> Directory {
        Directory {
            file: self.clone(),
            path: String::new(),
            keys: Arc::clone(&self.inner.root),
        }
    }

    /// The keys of the root directory, every cycle, in file order.
    pub fn keys(&self) -> impl Iterator<Item = KeyInfo<'_>> {
        self.inner.root.iter().map(KeyInfo)
    }

    /// Irregularities tolerated while reading (lenient mode).
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.inner
            .diagnostics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn source(&self) -> &dyn ReadAt {
        &*self.source
    }

    /// The same file, reading its bytes from `source` instead: the async
    /// driver points handles at the bytes it has fetched.
    pub(crate) fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            source,
        }
    }

    pub(crate) fn max_object_bytes(&self) -> u64 {
        self.inner.options.limits.max_object_bytes
    }

    /// Opens an `RNTuple` by path (its anchor key, class `ROOT::RNTuple`).
    ///
    /// # Errors
    ///
    /// `NotFound`, `ClassMismatch` if the key is not an `RNTuple` anchor;
    /// `UnsupportedVersion` for another format epoch; `Unsupported` for
    /// unknown format features; read and format errors.
    pub fn rntuple(&self, path: &str) -> Result<RNTuple> {
        self.root().rntuple(path)
    }

    /// Opens a `TTree` (or a class derived from it, such as `TNtuple`) by
    /// path.
    ///
    /// # Errors
    ///
    /// As [`File::get_dyn`], plus `ClassMismatch` if the object is not a
    /// tree.
    pub fn tree(&self, path: &str) -> Result<Tree> {
        self.root().tree(path)
    }

    /// Records diagnostics, once each: reading the same thing again (or an
    /// async read retried after fetching more bytes) does not repeat them.
    pub(crate) fn add_diagnostics(&self, more: Vec<Diagnostic>) {
        if !more.is_empty() {
            let mut all = self
                .inner
                .diagnostics
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for d in more {
                if !all.contains(&d) {
                    all.push(d);
                }
            }
        }
    }

    /// Reads an object by path (`"dir/sub/name"`, optionally `";cycle"`)
    /// into its typed model.
    ///
    /// # Errors
    ///
    /// As [`File::get_dyn`], plus `ClassMismatch` if the object is not of
    /// the model's class.
    pub fn get<T: FromDyn>(&self, path: &str) -> Result<T> {
        self.root().get(path)
    }

    /// Reads an object by path, decoded from its class description.
    ///
    /// # Errors
    ///
    /// `NotFound`, `InvalidPath`; `Unsupported` for classes this version
    /// cannot decode; read, decompression and format errors.
    pub fn get_dyn(&self, path: &str) -> Result<DynObject> {
        self.root().get_dyn(path)
    }

    /// The class descriptions stored in the file.
    ///
    /// # Errors
    ///
    /// Read and format errors of the `StreamerInfo` record.
    pub fn streamer_infos(&self) -> Result<Vec<StreamerInfo>> {
        Ok(self
            .streamer_set()?
            .iter()
            .map(|i| StreamerInfo::clone(i))
            .collect())
    }

    pub(crate) fn streamer_set(&self) -> Result<Arc<StreamerSet>> {
        if let Some(set) = self.inner.streamers.get() {
            return Ok(Arc::clone(set));
        }
        let h = &self.inner.header;
        let set = if h.seek_info.get() == 0 || h.nbytes_info == 0 {
            StreamerSet::default()
        } else {
            let at = h.seek_info;
            let rec = self
                .source
                .read_at(at..at.checked_add(u64::from(h.nbytes_info)).unwrap_or(at))?;
            let key = KeyHeader::decode(&mut RBuffer::new(&rec, 0).at(at))?;
            let data = self.read_object(&key, "StreamerInfo")?;
            let (infos, diagnostics) = streamer::decode_list(&data, u32::from(key.key_len))
                .context(Context::Key("StreamerInfo".to_owned()))?;
            self.add_diagnostics(diagnostics);
            StreamerSet::new(infos)
        };
        Ok(Arc::clone(
            self.inner.streamers.get_or_init(|| Arc::new(set)),
        ))
    }

    pub(crate) fn on_mismatch(&self) -> OnMismatch {
        match self.inner.options.strictness {
            Strictness::Lenient => OnMismatch::Reposition,
            Strictness::Strict => OnMismatch::Fail,
        }
    }

    /// Reads and decompresses a record's object data.
    fn read_object(&self, key: &KeyHeader, path: &str) -> Result<Vec<u8>> {
        let limit = self.inner.options.limits.max_object_bytes;
        if u64::from(key.obj_len) > limit {
            return Err(Error::new(
                ErrorKind::LimitExceeded,
                format!(
                    "object of {} bytes exceeds the {limit}-byte limit",
                    key.obj_len
                ),
            ))
            .context(Context::Key(path.to_owned()));
        }
        let rec = self.source.read_at(key.record_range())?;
        let data = rec
            .get(usize::from(key.key_len)..)
            .ok_or_else(|| Error::malformed("record shorter than its key"))?;
        let obj_len =
            usize::try_from(key.obj_len).map_err(|_| Error::malformed("ObjLen too large"))?;
        let out = if key.is_compressed() {
            compress::decompress(data, obj_len)
        } else {
            Ok(data.to_vec())
        };
        out.context(Context::Offset(key.seek_key))
            .context(Context::Key(path.to_owned()))
    }

    /// Walks every record in the file and checks it: key headers, gap
    /// markers, and decompression of every record's data.
    ///
    /// # Errors
    ///
    /// Only I/O errors of the source; format problems are collected in the
    /// returned report.
    pub fn check(&self) -> Result<CheckReport> {
        let h = &self.inner.header;
        let end = FileOffset::new(h.end.get().min(self.inner.len));
        let mut report = CheckReport::default();
        let free = if h.seek_free.get() > 0 {
            match self.read_free_list() {
                Ok(segments) => segments,
                Err(e) => {
                    report.errors.push((h.seek_free, format!("free list: {e}")));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        report.free_segments = u64::try_from(free.len()).unwrap_or(u64::MAX);
        let (steps, stopped) = walk(&*self.source, h.begin(), end, &free);
        report.stopped = stopped.map(|e| e.to_string());
        for s in steps {
            match s {
                Step::Gap { len, .. } => {
                    report.gaps += 1;
                    report.gap_bytes += len;
                }
                // An RNTuple blob holds what its metadata describes, not
                // one object: ROOT packs a cluster's pages, each compressed
                // on its own and followed by its checksum, into one blob
                // whose `ObjLen` is their uncompressed total. Its bytes
                // only have to be there; `rntuple` reads check the pages.
                Step::Record(key) if key.class_name == "RBlob" => {
                    report.records += 1;
                    let end = key.seek_key.checked_add(u64::from(key.nbytes));
                    if let Err(e) = end
                        .ok_or_else(|| Error::malformed("record past the end"))
                        .and_then(|end| Ok(self.source.read_at(key.seek_key..end)?))
                    {
                        report.errors.push((key.seek_key, e.to_string()));
                    }
                }
                Step::Record(key) => {
                    report.records += 1;
                    if key.is_compressed() {
                        report.compressed += 1;
                    }
                    let label = format!("{};{} ({})", key.name, key.cycle, key.class_name);
                    if let Err(e) = self.read_object(&key, &label) {
                        report.errors.push((key.seek_key, e.to_string()));
                    }
                }
            }
        }
        Ok(report)
    }

    /// The free list (update mode keeps it).
    pub(crate) fn free_list(&self) -> Result<Vec<FreeSegment>> {
        if self.inner.header.seek_free.get() == 0 {
            return Ok(Vec::new());
        }
        self.read_free_list()
    }

    /// The record at `seek` (`nbytes` long): its key and decompressed
    /// object.
    pub(crate) fn raw_record(&self, seek: FileOffset, nbytes: u32) -> Result<RawRecord> {
        let rec = self
            .source
            .read_at(seek..seek.checked_add(u64::from(nbytes)).unwrap_or(seek))?;
        let mut r = RBuffer::new(&rec, 0).at(seek);
        let key = KeyHeader::decode(&mut r)?;
        let object = self.read_object(&key, &key.name)?;
        Ok(RawRecord {
            key_len: key.key_len,
            object,
        })
    }

    fn read_free_list(&self) -> Result<Vec<FreeSegment>> {
        let h = &self.inner.header;
        let at = h.seek_free;
        let rec = self
            .source
            .read_at(at..at.checked_add(u64::from(h.nbytes_free)).unwrap_or(at))?;
        let mut r = RBuffer::new(&rec, 0).at(at);
        let key = KeyHeader::decode(&mut r)?;
        let segments = decode_free_list(&mut RBuffer::new(
            r.take(usize::try_from(key.data_len()).unwrap_or(0))?,
            u32::from(key.key_len),
        ))?;
        for s in &segments {
            if s.first > s.last {
                return Err(Error::malformed(format!(
                    "inverted free segment {}..{}",
                    s.first, s.last
                )));
            }
        }
        Ok(segments)
    }
}

/// The result of [`File::check`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckReport {
    /// Records walked.
    pub records: u64,
    /// Records whose data is compressed.
    pub compressed: u64,
    /// Free gaps found between records.
    pub gaps: u64,
    /// Total bytes in free gaps.
    pub gap_bytes: u64,
    /// Segments in the free list.
    pub free_segments: u64,
    /// Records that failed to decode or decompress, with their offsets.
    pub errors: Vec<(FileOffset, String)>,
    /// Why the walk stopped early, if it did.
    pub stopped: Option<String>,
}

impl CheckReport {
    /// Whether everything checked out.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && self.stopped.is_none()
    }
}

fn read_keys(
    source: &dyn ReadAt,
    dir: &DirectoryRecord,
    len: u64,
    end: FileOffset,
) -> Result<Vec<KeyHeader>> {
    if end.get() > len {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            format!("file is truncated (fEND {end}, size {len})"),
        ));
    }
    if dir.seek_keys.get() == 0 {
        return Err(Error::malformed("the directory has no key list"));
    }
    let start = dir.seek_keys;
    let rec = source.read_at(
        start
            ..start
                .checked_add(u64::from(dir.nbytes_keys))
                .unwrap_or(start),
    )?;
    let mut r = RBuffer::new(&rec, 0).at(start);
    let key = KeyHeader::decode(&mut r)?;
    let data = rec
        .get(usize::from(key.key_len)..)
        .ok_or_else(|| Error::malformed("key list shorter than its key"))?;
    let decompressed;
    let data = if key.is_compressed() {
        decompressed = compress::decompress(
            data,
            usize::try_from(key.obj_len).map_err(|_| Error::malformed("ObjLen"))?,
        )?;
        &decompressed[..]
    } else {
        data
    };
    decode_key_list(&mut RBuffer::new(data, u32::from(key.key_len)), len)
        .context(Context::Offset(start))
}

/// Rebuilds the root directory's keys by walking the records, as ROOT's
/// `TFile::Recover` does: every record whose parent is the root directory,
/// except bookkeeping records.
fn recover(
    source: &dyn ReadAt,
    header: &FileHeader,
    root: &DirectoryRecord,
    len: u64,
) -> Vec<KeyHeader> {
    // The free list of an unclosed file is not trustworthy: walk without it.
    let (steps, _) = walk(source, header.begin(), FileOffset::new(len), &[]);
    steps
        .into_iter()
        .filter_map(|s| match s {
            Step::Record(k) => Some(k),
            Step::Gap { .. } => None,
        })
        .filter(|k| {
            k.seek_pdir == root.seek_dir
                && k.seek_key != root.seek_keys
                && k.seek_key != header.seek_info
                && k.seek_key != header.seek_free
                && k.class_name != "TBasket"
                && k.class_name != "RBlob"
        })
        .collect()
}

/// A record's key length and decompressed object.
pub(crate) struct RawRecord {
    pub(crate) key_len: u16,
    pub(crate) object: Vec<u8>,
}

/// A directory of a file.
#[derive(Clone)]
pub struct Directory {
    file: File,
    path: String,
    keys: Arc<Vec<KeyHeader>>,
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory")
            .field("path", &self.path)
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

impl Directory {
    /// The path of this directory from the root (`""` for the root).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The key headers, as stored.
    pub(crate) fn raw_keys(&self) -> &[KeyHeader] {
        &self.keys
    }

    /// A subdirectory's record and key headers (update mode loads them).
    pub(crate) fn dir_raw(&self, path: &str) -> Result<(DirectoryRecord, Vec<KeyHeader>)> {
        let (parent, name) = match path.rsplit_once('/') {
            Some((p, n)) => (self.subdir(p)?, n),
            None => (self.clone(), path),
        };
        let key = parent
            .key(name, None)
            .filter(KeyInfo::is_directory)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no directory `{path}`")))?
            .0
            .clone();
        let data = parent.file.read_object(&key, path)?;
        let record = DirectoryRecord::decode(
            &mut RBuffer::new(&data, u32::from(key.key_len)).at(key.seek_key),
        )?;
        let keys = self.subdir(path)?.keys.to_vec();
        Ok((record, keys))
    }

    /// The keys of this directory, every cycle, in file order.
    pub fn keys(&self) -> impl Iterator<Item = KeyInfo<'_>> {
        self.keys.iter().map(KeyInfo)
    }

    /// Finds a key by name; without a cycle, the highest one.
    pub fn key(&self, name: &str, cycle: Option<u16>) -> Option<KeyInfo<'_>> {
        let matching = self.keys.iter().filter(|k| k.name == name);
        match cycle {
            Some(c) => matching
                .into_iter()
                .find(|k| k.cycle.unsigned_abs() == c)
                .map(KeyInfo),
            None => matching.max_by_key(|k| k.cycle.unsigned_abs()).map(KeyInfo),
        }
    }

    /// Opens a subdirectory, by a `/`-separated path relative to this one.
    ///
    /// # Errors
    ///
    /// `NotFound` if a component is missing or not a directory; read and
    /// format errors of the directory records.
    pub fn subdir(&self, path: &str) -> Result<Directory> {
        let mut dir = self.clone();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let (name, cycle) = split_cycle(part)?;
            let key = dir
                .key(name, cycle)
                .filter(KeyInfo::is_directory)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no directory `{part}`")))?
                .0
                .clone();
            let full = if dir.path.is_empty() {
                name.to_owned()
            } else {
                format!("{}/{name}", dir.path)
            };
            let data = dir.file.read_object(&key, &full)?;
            let record = DirectoryRecord::decode(
                &mut RBuffer::new(&data, u32::from(key.key_len)).at(key.seek_key),
            )
            .context(Context::Key(full.clone()))?;
            // ROOT writes no key list for an empty directory.
            let keys = if record.seek_keys.get() == 0 {
                Vec::new()
            } else {
                read_keys(
                    &*dir.file.source,
                    &record,
                    dir.file.inner.len,
                    dir.file.inner.header.end,
                )
                .context(Context::Key(full.clone()))?
            };
            dir = Directory {
                file: dir.file.clone(),
                path: full,
                keys: Arc::new(keys),
            };
        }
        Ok(dir)
    }

    /// Reads an object by path relative to this directory, into its typed
    /// model. See [`File::get`].
    ///
    /// # Errors
    ///
    /// As [`File::get`].
    pub fn get<T: FromDyn>(&self, path: &str) -> Result<T> {
        let obj = self.get_dyn(path)?;
        T::from_dyn(&obj).context(Context::Key(self.full_path(path)))
    }

    /// Reads an object by path relative to this directory, decoded from its
    /// class description. See [`File::get_dyn`].
    ///
    /// # Errors
    ///
    /// As [`File::get_dyn`].
    pub fn get_dyn(&self, path: &str) -> Result<DynObject> {
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (self.subdir(dir)?, name),
            None => (self.clone(), path),
        };
        let full = dir.full_path(name);
        let (base, cycle) = split_cycle(name)?;
        let key = dir.key(base, cycle).ok_or_else(|| dir.missing(name))?.0;
        if key.is_directory() {
            return Err(Error::new(
                ErrorKind::InvalidPath,
                format!("`{full}` is a directory; use `subdir`"),
            ));
        }
        let file = &dir.file;
        let data = file.read_object(key, &full)?;
        let set = file.streamer_set()?;
        let mut decoder = Decoder::new(&set, streamer::builtin(), file.on_mismatch());
        let mut r = RBuffer::new(&data, u32::from(key.key_len));
        if !key.is_compressed()
            && let Some(at) = key.seek_key.checked_add(u64::from(key.key_len))
        {
            r = r.at(at);
        }
        let mut obj = decoder
            .object(&mut r, &key.class_name)
            .context(Context::Key(full))?;
        file.add_diagnostics(decoder.into_diagnostics());
        obj.set_source(set);
        Ok(obj)
    }

    /// Opens an `RNTuple` by path relative to this directory; see
    /// [`File::rntuple`].
    ///
    /// # Errors
    ///
    /// As [`File::rntuple`].
    pub fn rntuple(&self, path: &str) -> Result<RNTuple> {
        let (dir, name) = match path.rsplit_once('/') {
            Some((d, n)) => (self.subdir(d)?, n),
            None => (self.clone(), path),
        };
        let (base, cycle) = split_cycle(name)?;
        let key = dir.key(base, cycle).ok_or_else(|| dir.missing(name))?;
        if key.class_name() != "ROOT::RNTuple" {
            return Err(Error::new(
                ErrorKind::ClassMismatch,
                format!("`{name}` is a {}, not an RNTuple", key.class_name()),
            ));
        }
        let anchor = dir.read_raw(name)?;
        RNTuple::open(self.file.clone(), dir.full_path(name), &anchor)
            .map_err(|e| e.with_context(Context::Key(dir.full_path(name))))
    }

    /// Opens a `TTree` by path relative to this directory; see
    /// [`File::tree`].
    ///
    /// # Errors
    ///
    /// As [`File::tree`].
    pub fn tree(&self, path: &str) -> Result<Tree> {
        let obj = self.get_dyn(path)?;
        Tree::from_dyn(self.file.clone(), self.full_path(path), &obj)
    }

    /// The same directory, reading through `source` (see
    /// [`File::with_source`]).
    #[cfg(feature = "async")]
    pub(crate) fn with_source(&self, source: Arc<dyn ReadAt>) -> Self {
        Self {
            file: self.file.with_source(source),
            path: self.path.clone(),
            keys: Arc::clone(&self.keys),
        }
    }

    /// The error for a name this directory has no key for: it names the
    /// directory searched.
    fn missing(&self, name: &str) -> Error {
        let place = if self.path.is_empty() {
            "the top directory".to_owned()
        } else {
            format!("directory `{}`", self.path)
        };
        Error::new(ErrorKind::NotFound, format!("no key `{name}` in {place}"))
    }

    fn full_path(&self, name: &str) -> String {
        if self.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.path)
        }
    }

    /// Reads an object's decompressed bytes (its streamed form), by name with
    /// an optional `;cycle`.
    ///
    /// # Errors
    ///
    /// `NotFound`, `InvalidPath`, limits, read and decompression errors.
    pub fn read_raw(&self, name: &str) -> Result<Vec<u8>> {
        let (name, cycle) = split_cycle(name)?;
        let key = self.key(name, cycle).ok_or_else(|| self.missing(name))?;
        let full = if self.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.path)
        };
        self.file.read_object(key.0, &full)
    }
}

fn split_cycle(part: &str) -> Result<(&str, Option<u16>)> {
    match part.split_once(';') {
        None => Ok((part, None)),
        Some((name, c)) => {
            let c: u16 = c.parse().map_err(|_| {
                Error::new(ErrorKind::InvalidPath, format!("bad cycle in `{part}`"))
            })?;
            if name.is_empty() || c == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidPath,
                    format!("bad object path `{part}`"),
                ));
            }
            Ok((name, Some(c)))
        }
    }
}

/// A key in a directory listing.
#[derive(Copy, Clone)]
pub struct KeyInfo<'a>(&'a KeyHeader);

impl KeyInfo<'_> {
    /// The object's name.
    pub fn name(&self) -> &str {
        &self.0.name
    }
    /// The object's title.
    pub fn title(&self) -> &str {
        &self.0.title
    }
    /// The object's class name.
    pub fn class_name(&self) -> &str {
        &self.0.class_name
    }
    /// The cycle number (absolute value; ROOT's "keep" cycles are negative).
    pub fn cycle(&self) -> u16 {
        self.0.cycle.unsigned_abs()
    }
    /// Whether the key holds a directory.
    pub fn is_directory(&self) -> bool {
        self.0.is_directory()
    }
    /// The record's position in the file.
    pub fn offset(&self) -> FileOffset {
        self.0.seek_key
    }
    /// Bytes on disk (key + data).
    pub fn stored_len(&self) -> u32 {
        self.0.nbytes
    }
    /// Uncompressed object size.
    pub fn object_len(&self) -> u32 {
        self.0.obj_len
    }
    /// When the record was written.
    pub fn datime(&self) -> crate::Datime {
        self.0.datime
    }
}

impl fmt::Debug for KeyInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyInfo")
            .field("name", &self.0.name)
            .field("cycle", &self.0.cycle)
            .field("class", &self.0.class_name)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for KeyInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{};{}  {}  {}",
            self.name(),
            self.cycle(),
            self.class_name(),
            self.title()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_parse() {
        assert_eq!(split_cycle("h").expect("ok"), ("h", None));
        assert_eq!(split_cycle("h;2").expect("ok"), ("h", Some(2)));
        assert_eq!(
            split_cycle("h;0").expect_err("zero").kind(),
            ErrorKind::InvalidPath
        );
        assert_eq!(
            split_cycle(";2").expect_err("empty").kind(),
            ErrorKind::InvalidPath
        );
        assert_eq!(
            split_cycle("h;x").expect_err("nan").kind(),
            ErrorKind::InvalidPath
        );
    }

    #[test]
    fn handles_are_thread_safe() {
        fn is<T: Send + Sync + 'static>() {}
        is::<File>();
        is::<Directory>();
    }
}
