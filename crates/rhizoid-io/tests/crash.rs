//! Update crash consistency:
//! the regression contract of update mode.
//!
//! An update runs against a recording sink, which logs every write and
//! every `sync()`. Then, at every crash point (between two syncs), the
//! harness builds the files a crash could leave: what was synced, plus
//! **every subset** of the writes since (all of them up to 8 pending
//! writes, 256 samples beyond), plus each pending write **torn** at a
//! 512-byte sector boundary. Each file must:
//!
//! - open cleanly (no recovery) and pass `check`;
//! - hold exactly the old content or exactly the new content, never a mix;
//! - take a **second update**, which exercises the free list left behind,
//!   and then hold that content plus what the second update added.
//!
//! The commit's order is asserted: everything synced, then the header
//! alone, synced, then the root directory record alone, synced. With
//! `RHIZOID_ROOT`, ROOT opens a sample of the files and reads every object.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rhizoid_io::objects::{TAxis, TH1D, TNamed};
use rhizoid_io::{Directory, File, FileOffset, Row, UpdateOptions, WritableFile, WriteAt};

#[derive(Clone, Debug)]
enum Op {
    Write(u64, Vec<u8>),
    Sync,
}

/// A sink over bytes that logs what it is asked to do.
struct Recorder {
    data: Arc<Mutex<Vec<u8>>>,
    log: Arc<Mutex<Vec<Op>>>,
}

fn apply(data: &mut Vec<u8>, at: u64, bytes: &[u8]) {
    let at = usize::try_from(at).expect("offset");
    if data.len() < at + bytes.len() {
        data.resize(at + bytes.len(), 0);
    }
    data[at..at + bytes.len()].copy_from_slice(bytes);
}

impl WriteAt for Recorder {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()> {
        apply(
            &mut self.data.lock().unwrap_or_else(PoisonError::into_inner),
            offset.get(),
            buf,
        );
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Op::Write(offset.get(), buf.to_vec()));
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Op::Sync);
        Ok(())
    }
}

#[derive(Row, Clone, Debug, PartialEq)]
struct X {
    x: i32,
}

/// The file before the update: objects, nested and empty directories, a
/// tree, a tree in a subdirectory.
fn base() -> Vec<u8> {
    let path = std::env::temp_dir().join(format!("rhizoid-crash-base-{}.root", std::process::id()));
    rhizoid_io::create(&path, |f| {
        f.put("n", &TNamed::new("n", "top"))?;
        let a = f.mkdir("a")?;
        a.put("n", &TNamed::new("n", "in a"))?;
        a.mkdir("b")?.put("n", &TNamed::new("n", "in a/b"))?;
        f.mkdir("c")?;
        let mut t = f.tree::<X>("t")?;
        t.extend(&(0..20).map(|x| X { x }).collect::<Vec<_>>())?;
        t.finish()?;
        let mut d = f.mkdir("d")?.tree::<X>("t")?;
        d.fill(&X { x: -1 })?;
        d.finish()
    })
    .expect("base");
    let bytes = std::fs::read(&path).expect("read");
    let _ = std::fs::remove_file(&path);
    bytes
}

/// The update under test: something of every kind.
fn first_update(f: &mut WritableFile) -> rhizoid_io::Result<()> {
    f.put("added", &TNamed::new("added", "at the top"))?;
    f.put("n", &TNamed::new("n", "top, cycle 2"))?;
    let b = f.mkdir("a/b")?;
    f.put_in(b, "u", &TNamed::new("u", "updated a/b"))?;
    let ef = f.mkdir("e/f")?;
    f.put_in(ef, "u", &TNamed::new("u", "new e/f"))?;
    // A class the file has no description of: the StreamerInfo grows.
    let h = TH1D::new("h", "histogram", TAxis::uniform(3, 0.0, 3.0)?)
        .with_values_flow(vec![0.0, 1.0, 2.0, 3.0, 0.0])?;
    f.put("h", &h)?;
    // An extended tree: a new cycle, the old one freed.
    let mut t = f.tree::<X>("t")?;
    for x in 20..30 {
        t.fill(&X { x })?;
    }
    t.finish()
}

/// What a file holds, as we read it; an error if it does not open cleanly.
fn content(bytes: &[u8]) -> Result<String, String> {
    fn walk(dir: &Directory, out: &mut String) -> rhizoid_io::Result<()> {
        let mut keys: Vec<(String, u16, String)> = dir
            .keys()
            .map(|k| (k.name().to_owned(), k.cycle(), k.class_name().to_owned()))
            .collect();
        keys.sort();
        for (name, cycle, class) in keys {
            let at = format!("{name};{cycle}");
            let _ = writeln!(out, "{}/{at} {class}", dir.path());
            match class.as_str() {
                "TDirectory" | "TDirectoryFile" => walk(&dir.subdir(&name)?, out)?,
                "TNamed" => {
                    let n: TNamed = dir.get(&at)?;
                    let _ = writeln!(out, "  {}", n.title());
                }
                "TH1D" => {
                    let h: TH1D = dir.get(&at)?;
                    let _ = writeln!(out, "  {:?}", h.values_flow());
                }
                "TTree" => {
                    let t = dir.tree(&at)?;
                    let x: Vec<i32> = t.branch::<i32>("x")?.read(..)?;
                    let _ = writeln!(out, "  {x:?} {:?}", t.clusters());
                }
                _ => {
                    dir.get_dyn(&at)?;
                }
            }
        }
        Ok(())
    }
    let options = rhizoid_io::ReadOptions::default().strictness(rhizoid_io::Strictness::Strict);
    let file = File::from_source(bytes.to_vec(), options).map_err(|e| e.to_string())?;
    if !file.diagnostics().is_empty() {
        return Err(format!("diagnostics: {:?}", file.diagnostics()));
    }
    let report = file.check().map_err(|e| e.to_string())?;
    if !report.is_ok() {
        return Err(format!("check: {report:?}"));
    }
    let mut s = String::new();
    walk(&file.root(), &mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

/// An in-memory sink that can sync.
struct Mem(Arc<Mutex<Vec<u8>>>);

impl WriteAt for Mem {
    fn write_all_at(&mut self, offset: FileOffset, buf: &[u8]) -> io::Result<()> {
        apply(
            &mut self.0.lock().unwrap_or_else(PoisonError::into_inner),
            offset.get(),
            buf,
        );
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A second update of `bytes`: adds `second` and a subdirectory.
fn second_update(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let data = Arc::new(Mutex::new(bytes.to_vec()));
    let mut f = WritableFile::update_in(
        bytes.to_vec(),
        Box::new(Mem(Arc::clone(&data))),
        UpdateOptions::default().reproducible(true),
    )
    .map_err(|e| e.to_string())?;
    f.put("second", &TNamed::new("second", "after a crash"))
        .map_err(|e| e.to_string())?;
    let g = f.mkdir("a/g").map_err(|e| e.to_string())?;
    f.put_in(g, "s", &TNamed::new("s", "second in a/g"))
        .map_err(|e| e.to_string())?;
    f.close().map_err(|e| e.to_string())?;
    let out = data.lock().unwrap_or_else(PoisonError::into_inner).clone();
    Ok(out)
}

/// A small deterministic generator for the sampled subsets.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Every file a crash during the logged update could leave.
fn crash_images(base: &[u8], log: &[Op]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut durable = base.to_vec();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let epochs: Vec<Vec<(u64, Vec<u8>)>> = log
        .split(|op| matches!(op, Op::Sync))
        .map(|ops| {
            ops.iter()
                .filter_map(|op| match op {
                    Op::Write(at, b) => Some((*at, b.clone())),
                    Op::Sync => None,
                })
                .collect()
        })
        .collect();
    for (e, pending) in epochs.iter().enumerate() {
        let n = pending.len();
        let subsets: Vec<Vec<bool>> = if n <= 8 {
            (0..1u32 << n)
                .map(|m| (0..n).map(|i| m >> i & 1 == 1).collect())
                .collect()
        } else {
            let mut s: Vec<Vec<bool>> = vec![vec![false; n], vec![true; n]];
            for k in 0..n {
                s.push((0..n).map(|i| i < k).collect()); // every prefix
            }
            for _ in 0..256 {
                s.push((0..n).map(|_| rng.next() & 1 == 1).collect());
            }
            s
        };
        for (si, subset) in subsets.iter().enumerate() {
            let mut image = durable.clone();
            for (w, keep) in pending.iter().zip(subset) {
                if *keep {
                    apply(&mut image, w.0, &w.1);
                }
            }
            out.push((format!("epoch {e} subset {si}"), image));
        }
        // Torn writes: the writes before, and a sector-aligned part of one.
        for (wi, (at, bytes)) in pending.iter().enumerate() {
            let end = at + bytes.len() as u64;
            let mut cuts: Vec<u64> = ((at / 512 + 1)..=((end - 1) / 512))
                .map(|s| s * 512)
                .collect();
            if cuts.len() > 4 {
                cuts = vec![cuts[0], cuts[cuts.len() / 2], cuts[cuts.len() - 1]];
            }
            for cut in cuts {
                let mut image = durable.clone();
                for w in &pending[..wi] {
                    apply(&mut image, w.0, &w.1);
                }
                let keep = usize::try_from(cut - at).expect("cut");
                apply(&mut image, *at, &bytes[..keep]);
                out.push((format!("epoch {e} write {wi} torn at {cut}"), image));
            }
        }
        for w in pending {
            apply(&mut durable, w.0, &w.1);
        }
    }
    out
}

#[test]
fn every_crash_leaves_the_old_file_or_the_new_one() {
    let base = base();
    let data = Arc::new(Mutex::new(base.clone()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Recorder {
        data: Arc::clone(&data),
        log: Arc::clone(&log),
    };
    let mut f = WritableFile::update_in(base.clone(), Box::new(sink), UpdateOptions::default())
        .expect("open");
    first_update(&mut f).expect("update");
    f.close().expect("commit");
    let new = data.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let log = log.lock().unwrap_or_else(PoisonError::into_inner).clone();

    let old_content = content(&base).expect("old");
    let new_content = content(&new).expect("new");
    assert_ne!(old_content, new_content);
    for line in ["a/b/u;1", "e/f/u;1", "/h;1 TH1D", "/n;2", "/t;2 TTree"] {
        assert!(new_content.contains(line), "{line}\n{new_content}");
    }
    assert!(
        !new_content.lines().any(|l| l.starts_with("/t;1 ")),
        "{new_content}"
    );

    // The commit's order: data synced, the header alone, synced, the root
    // directory record alone, synced.
    let header = FileOffset::new(0);
    let file = File::from_source(base.clone(), rhizoid_io::ReadOptions::default()).expect("open");
    let record_at = file.header().begin().get() + u64::from(file.header().nbytes_name());
    let epochs: Vec<&[Op]> = log.split(|op| matches!(op, Op::Sync)).collect();
    let alone = |at: u64| {
        epochs.iter().position(|e| {
            e.len() == 1 && matches!(&e[0], Op::Write(o, b) if *o == at && b.len() <= 100)
        })
    };
    let h = alone(header.get()).expect("the header written alone between syncs");
    let r = alone(record_at).expect("the root record written alone between syncs");
    assert_eq!(r, h + 1, "header, sync, root record");
    assert!(
        epochs[..h]
            .iter()
            .flat_map(|e| e.iter())
            .all(|op| match op {
                Op::Write(o, _) => *o != header.get() && *o != record_at,
                Op::Sync => true,
            }),
        "nothing referenced is written before the commit"
    );
    for (at, len) in [(header.get(), 100), (record_at, 60)] {
        assert_eq!(
            at / 512,
            (at + len - 1) / 512,
            "a commit write inside one sector"
        );
    }

    let old_after = content(&second_update(&base).expect("second of old")).expect("old+");
    let new_after = content(&second_update(&new).expect("second of new")).expect("new+");
    let images = crash_images(&base, &log);
    assert!(images.len() > 100, "{} images", images.len());
    let mut distinct = HashSet::new();
    let mut sample = Vec::new();
    for (what, image) in &images {
        let got = content(image)
            .map_err(|e| format!("{what}: {e}"))
            .expect("a crash image");
        assert!(
            got == old_content || got == new_content,
            "{what}: neither old nor new:\n{got}"
        );
        let was_new = got == new_content;
        let after = second_update(image)
            .map_err(|e| format!("{what}: second update: {e}"))
            .expect("a crash image");
        let after = content(&after)
            .map_err(|e| format!("{what}: after the second: {e}"))
            .expect("a crash image");
        assert_eq!(
            &after,
            if was_new { &new_after } else { &old_after },
            "{what}: the second update"
        );
        if distinct.insert(image.clone()) && sample.len() < 60 {
            sample.push((what.clone(), image.clone(), was_new));
        }
    }
    // Six crash points: the session's writes, the header, the root record,
    // the garbage released, its header, and after.
    assert_eq!(
        log.iter().filter(|op| matches!(op, Op::Sync)).count() + 1,
        6
    );
    assert!(
        images.len() >= 300 && distinct.len() >= 300,
        "{}",
        images.len()
    );
    root_opens(&sample);
}

/// With `RHIZOID_ROOT`: ROOT opens each sampled file, without recovery,
/// and reads every object.
fn root_opens(sample: &[(String, Vec<u8>, bool)]) {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhizoid-crash-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let mut list = String::new();
    for (i, (_, image, _)) in sample.iter().enumerate() {
        let p = dir.join(format!("image{i}.root"));
        std::fs::write(&p, image).expect("write");
        let _ = writeln!(list, "{}", p.display());
    }
    let list_path = dir.join("images.txt");
    std::fs::write(&list_path, list).expect("list");
    let script = dir.join("open_all.C");
    std::fs::write(
        &script,
        r#"#include <fstream>
static int readall(TDirectory *d) {
  int n = 0;
  for (auto k : *d->GetListOfKeys()) {
    auto key = static_cast<TKey *>(k);
    auto o = key->ReadObj();
    if (!o) return -1000;
    if (auto sub = dynamic_cast<TDirectory *>(o)) { int m = readall(sub); if (m < 0) return m; n += m; }
    if (auto t = dynamic_cast<TTree *>(o)) for (Long64_t i = 0; i < t->GetEntries(); ++i) if (t->GetEntry(i) <= 0) return -2000;
    ++n;
  }
  return n;
}
void open_all(const char *list) {
  std::ifstream in(list);
  std::string p;
  while (std::getline(in, p)) {
    auto f = TFile::Open(p.c_str());
    if (!f || f->IsZombie()) { printf("BAD %s zombie\n", p.c_str()); continue; }
    if (f->TestBit(TFile::kRecovered)) { printf("BAD %s recovered\n", p.c_str()); continue; }
    int n = readall(f);
    printf("%s %s %d\n", n < 0 ? "BAD" : "OK", p.c_str(), n);
    delete f;
  }
}
"#,
    )
    .expect("macro");
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l"])
        .arg(format!(
            "{}(\"{}\")",
            script.display(),
            Path::new(&list_path).display()
        ))
        .output()
        .expect("run root");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stdout.contains("BAD") && !stderr.contains("Error in <"),
        "{stdout}{stderr}"
    );
    let ok = stdout.lines().filter(|l| l.starts_with("OK ")).count();
    assert_eq!(ok, sample.len(), "{stdout}{stderr}");
}
