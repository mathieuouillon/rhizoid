//! Update mode: adding to an existing file with one commit point.
//!
//! Nothing the old file references is written over before the commit. New
//! records go past the old end of the file. A directory that changed is
//! written **copy-on-write**: a new key list, and for a subdirectory a new
//! key holding its record, at new places; its parent then changes too, up
//! to the root. The commit is two ordered writes, each inside one 512-byte
//! sector: the header (which only grows `fEND` and points at a superset of
//! the old `StreamerInfo` and a free list that frees nothing of this
//! session), then the root directory record. Before the first, the file
//! reads as it was; between them, it still does (the old root directory
//! under the new header); after the second, it reads as updated.
//!
//! What the session made garbage (old key lists, directory keys,
//! `StreamerInfo`, free list, replaced tree cycles) is listed as free only
//! after that, by a second header write that can only lose it (a leak),
//! never give it away while it is referenced.

use std::path::{Path, PathBuf};

use rhizoid_source::{FileOffset, WriteAt};

use super::{DIRECTORY_CLASS, DirState, WritableFile, WriteOptions};
use crate::compress::{Algorithm, Compression};
use crate::core::buffer::{RBuffer, WBuffer};
use crate::core::directory::{DirectoryRecord, FreeSegment};
use crate::core::error::{Error, ErrorKind, Result};
use crate::core::header::FileHeader;
use crate::core::key::{KeyForm, KeyHeader};
use crate::core::walk::{Step, walk};
use crate::file::{ReadOptions, Strictness};
use crate::streamer::{self, StreamerInfo};

/// The size of a sector written atomically: each commit write must lie
/// inside one.
const SECTOR: u64 = 512;

/// Options for [`update`](super::update).
#[derive(Clone, Copy, Debug, Default)]
pub struct UpdateOptions {
    copy_on_write: bool,
    compression: Option<Compression>,
    reproducible: bool,
}

impl UpdateOptions {
    /// Applies the update to a copy of the file, then renames the copy over
    /// the original (and, on POSIX, syncs the directory). For irreplaceable
    /// files, and for storage that does not write 512-byte sectors
    /// atomically (network and FUSE filesystems). It needs room for a copy.
    /// The default is `false`.
    #[must_use]
    pub fn copy_on_write(mut self, yes: bool) -> Self {
        self.copy_on_write = yes;
        self
    }

    /// The compression of what this update writes. The default is the
    /// file's own setting when it can be written (zlib, LZ4), else zlib
    /// level 1.
    #[must_use]
    pub fn compression(mut self, c: Compression) -> Self {
        self.compression = Some(c);
        self
    }

    /// Writes ROOT's `kReproducible` values (datimes 1, zero UUIDs) in the
    /// records this update adds. The default is `false`.
    #[must_use]
    pub fn reproducible(mut self, yes: bool) -> Self {
        self.reproducible = yes;
        self
    }
}

/// What an update knows of the file as it was.
pub(crate) struct UpdateState {
    /// The file as it was: every read of old content goes through it.
    pub(crate) reader: crate::File,
    /// The old header.
    pub(crate) header: FileHeader,
    /// The old end of the file: records before it are old.
    pub(crate) old_end: FileOffset,
    /// The old free list's segments inside the file, checked against the
    /// records (the tail segment from `fEND` is rebuilt).
    pub(crate) free: Vec<FreeSegment>,
    /// The old free list record.
    pub(crate) free_record: (FileOffset, u32),
    /// Records this session makes garbage, freed once committed.
    pub(crate) freed: Vec<(FileOffset, u32)>,
    /// The class descriptions the file has, by name and version.
    pub(crate) old_infos: Vec<(String, i32)>,
    /// For `copy_on_write`: the copy being updated and the original.
    pub(crate) rename: Option<(PathBuf, PathBuf)>,
    /// The locked original (released when the update ends).
    lock: Option<LockGuard>,
}

/// Holds the file's lock, and releases it explicitly when dropped: on Unix
/// a lock goes with the open file, which a child process forked at that
/// moment by another thread still holds until it execs.
struct LockGuard(std::fs::File);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn locked() -> Error {
    Error::new(
        ErrorKind::Io,
        "the file is being written by another writer (it is locked)",
    )
}

/// Opens `path` for update.
pub(super) fn open(path: &Path, options: UpdateOptions) -> Result<WritableFile> {
    // The original is locked for the whole session, whichever file is
    // written.
    let original = std::fs::File::options().read(true).write(true).open(path)?;
    match original.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(locked()),
        Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
    }
    if options.copy_on_write {
        let name = path
            .file_name()
            .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
        let copy = path.with_file_name(format!(".{name}.rhizoid-update"));
        // Copied through the locked handle (Windows keeps others out).
        let mut sink = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&copy)?;
        std::io::copy(&mut &original, &mut sink)?;
        sink.sync_all()?;
        let reader = crate::File::open_with(&copy, strict())?;
        let mut file = open_sink(reader, Box::new(sink), options, true)?;
        if let Some(u) = file.update.as_mut() {
            u.rename = Some((copy, path.to_path_buf()));
            u.lock = Some(LockGuard(original));
        }
        return Ok(file);
    }
    // Reads and writes go through the locked handle: Windows' locks keep
    // every other handle out.
    let reader = crate::File::from_source(original.try_clone()?, strict())?;
    let sink = original.try_clone()?;
    let mut file = open_sink(reader, Box::new(sink), options, false)?;
    if let Some(u) = file.update.as_mut() {
        u.lock = Some(LockGuard(original));
    }
    Ok(file)
}

pub(super) fn strict() -> ReadOptions {
    ReadOptions::default().strictness(Strictness::Strict)
}

/// Opens a file for update from a reader of its current bytes and a sink
/// writing to the same file. (Tests drive the crash harness through it.)
pub(crate) fn open_sink(
    reader: crate::File,
    sink: Box<dyn WriteAt>,
    options: UpdateOptions,
    copy_on_write: bool,
) -> Result<WritableFile> {
    if !reader.diagnostics().is_empty() {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "the file was not closed cleanly: recover it (`rhizoid check`) before updating it",
        ));
    }
    let header = reader.header().clone();
    let begin = header.begin();
    let record_at = begin
        .checked_add(u64::from(header.nbytes_name))
        .ok_or_else(|| Error::malformed("fNbytesName"))?;
    let len = DirectoryRecord::LEN as u64;
    if !copy_on_write && record_at.get() / SECTOR != (record_at.get() + len - 1) / SECTOR {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "the root directory record crosses a 512-byte sector, so it cannot be committed \
             atomically: update with `UpdateOptions::copy_on_write(true)`",
        ));
    }
    let source = reader.source();
    let raw = source.read_at(record_at..FileOffset::new(record_at.get() + len))?;
    let record = DirectoryRecord::decode(&mut RBuffer::new(&raw, 0).at(record_at))?;
    let keys = reader.root().raw_keys().to_vec();

    // The free list, checked against the records: an interior segment is
    // kept only where the walk finds a gap (ROOT marks freed space).
    let old_free = reader.free_list()?;
    let (steps, _) = walk(source, begin, header.end, &[]);
    let mut gaps = Vec::new();
    let mut at = begin.get();
    for s in steps {
        match s {
            Step::Gap { len } => {
                gaps.push((at, at + len));
                at += len;
            }
            Step::Record(k) => at += u64::from(k.nbytes),
        }
    }
    let free = old_free
        .iter()
        .filter(|s| s.last < header.end)
        .filter(|s| {
            gaps.iter()
                .any(|&(a, b)| a <= s.first.get() && s.last.get() < b)
        })
        .copied()
        .collect();

    let old_infos = reader
        .streamer_infos()?
        .iter()
        .map(|i| (i.class_name().to_owned(), i.class_version()))
        .collect();
    let compression = options.compression.unwrap_or_else(|| {
        Compression::try_from(header.compress)
            .ok()
            .filter(|c| matches!(c.algorithm(), None | Some(Algorithm::Zlib | Algorithm::Lz4)))
            .unwrap_or_default()
    });
    compression.check_writable()?;
    let write_options = WriteOptions {
        compression,
        reproducible: options.reproducible,
        title: reader.title().to_owned(),
    };
    let root = DirState {
        name: reader.name().to_owned(),
        title: reader.title().to_owned(),
        parent: None,
        record,
        key: None,
        keys,
        dirty: false,
        new: false,
    };
    Ok(WritableFile {
        sink,
        options: write_options,
        name: reader.name().to_owned(),
        end: header.end,
        nbytes_name: header.nbytes_name,
        uuid: header.uuid,
        dirs: vec![root],
        classes: Vec::new(),
        infos: Vec::new(),
        sources: Vec::new(),
        pending: Vec::new(),
        pending_ntuples: Vec::new(),
        update: Some(Box::new(UpdateState {
            free_record: (header.seek_free, header.nbytes_free),
            old_end: header.end,
            header,
            free,
            freed: Vec::new(),
            old_infos,
            rename: None,
            lock: None,
            reader,
        })),
    })
}

/// Loads an existing subdirectory (its record and keys) from the file as
/// it was.
pub(super) fn load_dir(file: &WritableFile, parent: usize, key: &KeyHeader) -> Result<DirState> {
    let Some(u) = file.update.as_ref() else {
        return Err(Error::malformed("a directory missing from the session"));
    };
    let path = {
        let p = file.dir_path(parent);
        if p.is_empty() {
            key.name.clone()
        } else {
            format!("{p}/{}", key.name)
        }
    };
    let (record, keys) = u.reader.root().dir_raw(&path)?;
    Ok(DirState {
        name: key.name.clone(),
        title: key.title.clone(),
        parent: Some(parent),
        record,
        key: Some(key.clone()),
        keys,
        dirty: false,
        new: false,
    })
}

/// Commits an update.
#[expect(clippy::too_many_lines, reason = "the commit protocol, step by step")]
pub(super) fn commit(mut file: WritableFile) -> Result<()> {
    // Changes reach the root through every directory above them.
    for i in 0..file.dirs.len() {
        if file.dirs[i].dirty {
            let mut at = file.dirs[i].parent;
            while let Some(p) = at {
                file.dirs[p].dirty = true;
                at = file.dirs[p].parent;
            }
        }
    }
    if !file.dirs[0].dirty {
        // Nothing was added: the file stays as it is.
        return finish_rename(&mut file);
    }

    // PREPARE 1–2: every changed subdirectory that existed moves: reserve
    // its new key (parents first, so that children know where they are).
    let order = preorder(&file);
    let mut moved: Vec<Option<KeyHeader>> = vec![None; file.dirs.len()];
    for &d in &order {
        let dir = &file.dirs[d];
        if dir.parent.is_none() || dir.new || !dir.dirty {
            continue;
        }
        let Some(old) = dir.key.clone() else { continue };
        let parent_seek = seek_dir(&file, &moved, dir.parent.unwrap_or(0));
        let len = u32::try_from(DirectoryRecord::LEN).map_err(|_| Error::malformed("record"))?;
        let mut key = KeyHeader::for_record(
            KeyForm::ByPosition,
            file.end,
            parent_seek,
            DIRECTORY_CLASS,
            &dir.name,
            &dir.title,
            len,
            len,
            old.cycle,
            file.now(),
        )?;
        key.cycle = old.cycle;
        file.end = file
            .end
            .checked_add(u64::from(key.nbytes))
            .ok_or_else(|| Error::new(ErrorKind::LimitExceeded, "file too large"))?;
        moved[d] = Some(key);
    }

    // PREPARE 2–3: key lists and records, children first. Parents find
    // their moved children's entries by the keys they had.
    let original: Vec<Option<KeyHeader>> = file.dirs.iter().map(|d| d.key.clone()).collect();
    for &d in order.iter().rev() {
        if !file.dirs[d].dirty {
            continue;
        }
        let here = seek_dir(&file, &moved, d);
        // Entries of moved children point at their new keys; every entry
        // names this directory's (new) place as its parent.
        let children: Vec<(usize, KeyHeader)> = (0..file.dirs.len())
            .filter(|&c| file.dirs[c].parent == Some(d))
            .filter_map(|c| moved[c].clone().map(|k| (c, k)))
            .collect();
        for (c, new_key) in children {
            if let Some(old) = original[c].clone()
                && let Some(entry) = file.dirs[d]
                    .keys
                    .iter_mut()
                    .find(|k| k.seek_key == old.seek_key)
            {
                *entry = new_key;
            }
        }
        for k in &mut file.dirs[d].keys {
            k.seek_pdir = here;
        }
        let old_list = (
            file.dirs[d].record.seek_keys,
            file.dirs[d].record.nbytes_keys,
        );
        let fresh = file.dirs[d].new;
        let (seek, nbytes) = file.write_key_list(d, here)?;
        if !fresh && old_list.0.get() != 0 {
            freed(&mut file, old_list.0, old_list.1);
        }
        let now = file.now();
        let parent_seek = file.dirs[d]
            .parent
            .map_or(FileOffset::new(0), |p| seek_dir(&file, &moved, p));
        let dir = &mut file.dirs[d];
        dir.record.seek_keys = seek;
        dir.record.nbytes_keys = nbytes;
        dir.record.modified = now;
        if dir.parent.is_none() {
            continue; // the root record is the commit write
        }
        dir.record.seek_parent = parent_seek;
        if let Some(new_key) = moved[d].clone() {
            // Copy-on-write: the record in the new key.
            let old = dir.key.replace(new_key.clone());
            dir.record.seek_dir = new_key.seek_key;
            dir.record.nbytes_name = u32::from(new_key.key_len);
            let mut w = WBuffer::new();
            new_key.encode(&mut w)?;
            dir.record.encode(&mut w)?;
            let bytes = w.into_inner();
            file.sink.write_all_at(new_key.seek_key, &bytes)?;
            if let Some(old) = old {
                freed(&mut file, old.seek_key, old.nbytes);
            }
        } else {
            // Created by this session: not referenced yet, rewritten in place.
            let mut w = WBuffer::new();
            dir.record.encode(&mut w)?;
            let at = dir.record_at()?;
            let bytes = w.into_inner();
            file.sink.write_all_at(at, &bytes)?;
        }
    }

    // PREPARE 4: StreamerInfo = the old list and what this session adds.
    let (seek_info, nbytes_info) = streamer_info_union(&mut file)?;

    // PREPARE 5: the free list frees nothing of this session.
    let (free, old_free) = {
        let u = file
            .update
            .as_ref()
            .ok_or_else(|| Error::malformed("update"))?;
        (u.free.clone(), u.free_record)
    };
    let (seek_free, nbytes_free, nfree) = file.write_free_list(&free)?;
    freed(&mut file, old_free.0, old_free.1);

    // PREPARE 6: every byte above is durable.
    sync(&mut file)?;

    // COMMIT 7: the header, then 8: the root directory record.
    let header = file.header(seek_free, nbytes_free, seek_info, nbytes_info, nfree)?;
    file.sink.write_all_at(FileOffset::new(0), &header)?;
    sync(&mut file)?;
    let mut w = WBuffer::new();
    file.dirs[0].record.encode(&mut w)?;
    let at = file.dirs[0].record_at()?;
    file.sink.write_all_at(at, &w.into_inner())?;
    sync(&mut file)?;

    // AFTER 9: what this session made garbage is free.
    release(&mut file, &free, seek_info, nbytes_info)?;
    finish_rename(&mut file)
}

/// Where directory `d` is (or will be, once committed).
fn seek_dir(file: &WritableFile, moved: &[Option<KeyHeader>], d: usize) -> FileOffset {
    moved
        .get(d)
        .and_then(Option::as_ref)
        .map_or(file.dirs[d].record.seek_dir, |k| k.seek_key)
}

/// Directories, parents before children.
fn preorder(file: &WritableFile) -> Vec<usize> {
    let mut out = vec![0];
    let mut i = 0;
    while i < out.len() {
        let d = out[i];
        out.extend((0..file.dirs.len()).filter(|&c| file.dirs[c].parent == Some(d)));
        i += 1;
    }
    out
}

fn freed(file: &mut WritableFile, at: FileOffset, nbytes: u32) {
    if let Some(u) = file.update.as_mut()
        && at.get() != 0
        && nbytes > 0
        && at < u.old_end
    {
        u.freed.push((at, nbytes));
    }
}

fn sync(file: &mut WritableFile) -> Result<()> {
    file.sink.flush()?;
    file.sink.sync().map_err(|e| {
        if e.kind() == std::io::ErrorKind::Unsupported {
            Error::new(
                ErrorKind::Unsupported,
                "update needs a sink that can sync (its commit depends on it)",
            )
        } else {
            e.into()
        }
    })
}

/// The `StreamerInfo` record after this session: the old one if it already
/// describes every class written, else the old list with the new
/// descriptions appended (its other entries, such as schema evolution
/// rules, kept byte for byte).
fn streamer_info_union(file: &mut WritableFile) -> Result<(FileOffset, u32)> {
    let u = file
        .update
        .as_ref()
        .ok_or_else(|| Error::malformed("update"))?;
    let (old_seek, old_nbytes) = (u.header.seek_info, u.header.nbytes_info);
    let needed = file.needed_infos();
    let new: Vec<&StreamerInfo> = needed
        .iter()
        .map(AsRef::as_ref)
        .filter(|i| {
            !u.old_infos
                .iter()
                .any(|(n, v)| n == i.class_name() && *v == i.class_version())
        })
        .collect();
    if new.is_empty() {
        return Ok((old_seek, old_nbytes));
    }
    let (info_class, info_name, info_title) = ("TList", "StreamerInfo", "Doubly linked list");
    let key_len = KeyHeader::encoded_len(info_class, info_name, info_title, file.end.is_large());
    let old = u.reader.raw_record(old_seek, old_nbytes)?;
    let list = if old.key_len as usize == key_len {
        streamer::append_to_list(&old.object, &new, u32::try_from(key_len).unwrap_or(0))?
    } else {
        // The key changed form (the file grew past 2 GB): re-encode, which
        // keeps only descriptions.
        let mut all: Vec<&StreamerInfo> = Vec::new();
        let kept = u.reader.streamer_infos()?;
        all.extend(kept.iter());
        all.extend(new.iter().copied());
        streamer::encode_list(&all, u32::try_from(key_len).unwrap_or(0))?
    };
    let compression = file.options.compression;
    let key = file.append_record(0, info_class, info_name, info_title, &list, compression, 1)?;
    freed(file, old_seek, old_nbytes);
    Ok((key.seek_key, key.nbytes))
}

/// After the commit: marks what this session made garbage as free space
/// (ROOT's gap markers) and lists it in a new free list, which a second
/// header write makes current.
fn release(
    file: &mut WritableFile,
    free: &[FreeSegment],
    seek_info: FileOffset,
    nbytes_info: u32,
) -> Result<()> {
    let Some(u) = file.update.as_ref() else {
        return Ok(());
    };
    let mut segments: Vec<FreeSegment> = free.to_vec();
    for &(at, n) in &u.freed {
        segments.push(FreeSegment {
            first: at,
            last: FileOffset::new(at.get() + u64::from(n) - 1),
        });
    }
    segments.sort_by_key(|s| s.first);
    let mut merged: Vec<FreeSegment> = Vec::new();
    for s in segments {
        match merged.last_mut() {
            Some(m) if s.first.get() <= m.last.get() + 1 => {
                m.last = m.last.max(s.last);
            }
            _ => merged.push(s),
        }
    }
    if merged == free {
        return Ok(());
    }
    // Gap markers: a negative size at the start of each free segment, so
    // that record walkers (ROOT's and ours) skip it.
    for s in &merged {
        let len = s.last.get() - s.first.get() + 1;
        if let Ok(n) = i32::try_from(len)
            && len >= 4
        {
            file.sink.write_all_at(s.first, &(-n).to_be_bytes())?;
        }
    }
    let (seek, nbytes, nfree) = file.write_free_list(&merged)?;
    sync(file)?;
    let header = file.header(seek, nbytes, seek_info, nbytes_info, nfree)?;
    file.sink.write_all_at(FileOffset::new(0), &header)?;
    sync(file)
}

/// For `copy_on_write`: the updated copy replaces the original.
fn finish_rename(file: &mut WritableFile) -> Result<()> {
    let Some((copy, original)) = file.update.as_mut().and_then(|u| u.rename.take()) else {
        return Ok(());
    };
    sync(file)?;
    // Windows refuses to replace a file locked by another handle.
    #[cfg(windows)]
    if let Some(u) = file.update.as_mut() {
        u.lock = None;
    }
    std::fs::rename(&copy, &original)?;
    #[cfg(unix)]
    if let Some(parent) = original.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
