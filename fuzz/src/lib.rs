//! Shared helpers of the fuzz targets: an in-memory sink whose bytes can be
//! read back, and a walk that reads everything a file holds.

use std::io;
use std::sync::{Arc, Mutex};

use rhizoid_io::{Directory, File, FileOffset, ReadLimits, ReadOptions, WriteAt};

/// The most keys, entries and directory levels one input may make us read,
/// so that every run stays fast whatever the file claims.
const MAX_KEYS: usize = 64;
const MAX_ENTRIES: u64 = 4096;
const MAX_DEPTH: usize = 4;

/// Read options for untrusted input: small objects only.
pub fn untrusted() -> ReadOptions {
    ReadOptions::default().limits(ReadLimits::untrusted().max_object_bytes(8 << 20))
}

/// Opens `data` as a ROOT file and reads everything in it through the
/// public API: every key and object, every tree's branches, every
/// `RNTuple`'s fields, and the integrity check. Errors are fine; a panic,
/// a hang or an unbounded allocation is a bug.
pub fn read_everything(data: &[u8]) {
    let Ok(file) = File::from_source(data.to_vec(), untrusted()) else {
        return;
    };
    let _ = file.diagnostics();
    let _ = file.streamer_infos();
    let _ = file.check();
    let mut budget = MAX_KEYS;
    walk(&file, &file.root(), 0, &mut budget);
}

fn walk(file: &File, dir: &Directory, depth: usize, budget: &mut usize) {
    let keys: Vec<(String, String, u16, bool)> = dir
        .keys()
        .map(|k| {
            (
                k.name().to_owned(),
                k.class_name().to_owned(),
                k.cycle(),
                k.is_directory(),
            )
        })
        .collect();
    for (name, class, cycle, is_dir) in keys {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = if dir.path().is_empty() {
            format!("{name};{cycle}")
        } else {
            format!("{}/{name};{cycle}", dir.path())
        };
        let _ = dir.read_raw(&name);
        if is_dir {
            if depth < MAX_DEPTH
                && let Ok(sub) = dir.subdir(&name)
            {
                walk(file, &sub, depth + 1, budget);
            }
            continue;
        }
        let _ = file.get_dyn(&path);
        match class.as_str() {
            "TTree" | "TNtuple" | "TNtupleD" => read_tree(file, &path),
            "ROOT::RNTuple" => read_rntuple(file, &path),
            _ => {}
        }
    }
}

fn read_tree(file: &File, path: &str) {
    let Ok(tree) = file.tree(path) else { return };
    let n = tree.num_entries().min(MAX_ENTRIES);
    let names: Vec<String> = tree
        .readable_branches()
        .iter()
        .map(|b| b.name().to_owned())
        .collect();
    for name in &names {
        let _ = tree.read(name, 0..n);
    }
    if let Ok(reader) = tree.reader().projection(names) {
        for batch in reader.build().take(4) {
            if batch.is_err() {
                break;
            }
        }
    }
}

fn read_rntuple(file: &File, path: &str) {
    let Ok(nt) = file.rntuple(path) else { return };
    let n = nt.num_entries().min(MAX_ENTRIES);
    let fields: Vec<String> = nt.fields().iter().map(|f| f.name().to_owned()).collect();
    for name in &fields {
        let _ = nt.read(name, 0..n);
    }
}

/// An in-memory sink whose bytes stay readable after the writer that owns
/// it is gone.
#[derive(Clone, Default)]
pub struct Shared(pub Arc<Mutex<Vec<u8>>>);

impl Shared {
    /// The bytes written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

impl WriteAt for Shared {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()> {
        let mut v = self.0.lock().map_err(|_| io::Error::other("poisoned"))?;
        v.write_all_at(offset, buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}
