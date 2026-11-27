//! Subdirectories and update mode (M9): what a file holds after an update,
//! checked with our reader and, with `RHIZOID_ROOT`, with ROOT 6.42, which
//! must read our updated files without a warning and update them itself
//! afterwards (`tests/cross/root_dirs.C`). The crash harness is in
//! `crash.rs`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rhizoid_io::objects::{TAxis, TH1D, TNamed};
use rhizoid_io::{Directory, ErrorKind, File, Row, UpdateOptions, WritableFile};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhizoid-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join(name)
}

fn copy_of(fixture: &str, name: &str) -> PathBuf {
    let path = out(name);
    std::fs::copy(data(fixture), &path).expect("copy");
    path
}

/// Everything a file holds, as we read it: every directory and key
/// (`path;cycle class`), every `TNamed`'s title, every tree's entries.
fn dump(path: &Path) -> String {
    fn walk(dir: &Directory, out: &mut String) {
        let mut keys: Vec<(String, u16, String)> = dir
            .keys()
            .map(|k| (k.name().to_owned(), k.cycle(), k.class_name().to_owned()))
            .collect();
        keys.sort();
        for (name, cycle, class) in keys {
            let full = if dir.path().is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", dir.path())
            };
            let _ = writeln!(out, "KEY {full};{cycle} {class}");
            match class.as_str() {
                "TDirectory" | "TDirectoryFile" => {
                    walk(&dir.subdir(&name).expect("subdir"), out);
                }
                "TNamed" => {
                    let n: TNamed = dir.get(&format!("{name};{cycle}")).expect("named");
                    let _ = writeln!(out, "NAMED {full} {}", n.title());
                }
                "TTree" => {
                    let t = dir.tree(&format!("{name};{cycle}")).expect("tree");
                    let x: Vec<i32> = t.branch::<i32>("x").expect("x").read(..).expect("read");
                    let _ = writeln!(out, "TREE {full} {x:?} {:?}", t.clusters());
                }
                _ => {
                    dir.get_dyn(&format!("{name};{cycle}")).expect("object");
                }
            }
        }
    }
    let file = File::open(path).expect("open");
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{}: {report:?}", path.display());
    assert!(file.diagnostics().is_empty(), "{:?}", file.diagnostics());
    let mut s = String::new();
    walk(&file.root(), &mut s);
    s
}

#[test]
fn subdirectories_are_written_and_read_back() {
    let path = out("dirs.root");
    rhizoid_io::create(&path, |f| {
        f.put("n", &TNamed::new("n", "top"))?;
        let a = f.mkdir("a")?;
        a.put("n", &TNamed::new("n", "in a"))?;
        a.mkdir("b")?.put("n", &TNamed::new("n", "in a/b"))?;
        f.mkdir("c")?;
        // mkdir finds what exists.
        f.mkdir("a/b")?.put("m", &TNamed::new("m", "also in a/b"))?;
        Ok::<_, rhizoid_io::Error>(())
    })
    .expect("write");
    assert_eq!(
        dump(&path),
        "KEY a;1 TDirectory\nKEY a/b;1 TDirectory\nKEY a/b/m;1 TNamed\nNAMED a/b/m also in a/b\n\
         KEY a/b/n;1 TNamed\nNAMED a/b/n in a/b\nKEY a/n;1 TNamed\nNAMED a/n in a\nKEY c;1 TDirectory\n\
         KEY n;1 TNamed\nNAMED n top\n"
    );
}

#[test]
fn we_read_roots_directories_empty_ones_included() {
    let s = dump(&data("root_dirs.root"));
    assert!(s.contains("KEY c;1 TDirectory\n"), "{s}");
    assert!(s.contains("NAMED a/b/n in a/b\n"), "{s}");
}

/// An update adds to the root, to existing subdirectories (moved
/// copy-on-write) and to new ones, keeping all that was there.
fn update_dirs(path: &Path) {
    rhizoid_io::update(path, |f| {
        f.put("added", &TNamed::new("added", "at the top"))?;
        f.mkdir("a/b")?.put("u", &TNamed::new("u", "updated a/b"))?;
        f.mkdir("e/f")?.put("u", &TNamed::new("u", "new e/f"))?;
        f.put("n", &TNamed::new("n", "top, cycle 2"))?;
        Ok::<_, rhizoid_io::Error>(())
    })
    .expect("update");
}

const ADDED: &str = "KEY a/b/u;1 TNamed\nNAMED a/b/u updated a/b\n";

#[test]
fn updates_keep_what_was_there() {
    for fixture in ["root_dirs.root", "ours"] {
        let path = if fixture == "ours" {
            let p = out("ours_dirs.root");
            rhizoid_io::create(&p, |f| {
                f.put("n", &TNamed::new("n", "top"))?;
                f.mkdir("a/b")?.put("n", &TNamed::new("n", "in a/b"))?;
                f.mkdir("c")?;
                Ok::<_, rhizoid_io::Error>(())
            })
            .expect("write");
            p
        } else {
            copy_of(fixture, "root_dirs_updated.root")
        };
        let before = dump(&path);
        update_dirs(&path);
        let after = dump(&path);
        for line in before.lines() {
            assert!(after.contains(line), "{fixture}: lost {line}\n{after}");
        }
        for line in [
            "KEY added;1 TNamed",
            "NAMED added at the top",
            "KEY e;1 TDirectory",
            "KEY e/f;1 TDirectory",
            "NAMED e/f/u new e/f",
            "KEY n;2 TNamed",
            "NAMED n top, cycle 2",
        ] {
            assert!(after.contains(line), "{fixture}: missing {line}\n{after}");
        }
        assert!(after.contains(ADDED), "{after}");
        // A second update finds what the first one moved.
        rhizoid_io::update(&path, |f| {
            f.mkdir("a/b")?.put("v", &TNamed::new("v", "second"))
        })
        .expect("second update");
        let again = dump(&path);
        assert!(again.contains("NAMED a/b/v second"), "{again}");
        assert!(again.contains(ADDED), "{again}");
    }
}

#[test]
fn a_failed_update_changes_nothing() {
    let path = copy_of("root_dirs.root", "failed.root");
    let original = std::fs::read(&path).expect("read");
    let before = dump(&path);
    let e = rhizoid_io::update(&path, |f| {
        f.put("x", &TNamed::new("x", "never committed"))?;
        f.mkdir("a/z")?;
        Err::<(), _>(rhizoid_io::Error::custom("the body failed"))
    })
    .expect_err("body error");
    assert!(e.to_string().contains("the body failed"), "{e}");
    let after = std::fs::read(&path).expect("read");
    // Only bytes past the old end may have been written.
    assert_eq!(&after[..original.len()], &original[..]);
    assert_eq!(dump(&path), before);
    // Dropping an uncommitted WritableFile does the same.
    let mut w = WritableFile::update(&path).expect("open");
    w.put("y", &TNamed::new("y", "dropped")).expect("put");
    drop(w);
    assert_eq!(dump(&path), before);
}

#[test]
fn new_classes_join_the_streamer_info() {
    let path = out("union.root");
    rhizoid_io::create(&path, |f| f.put("n", &TNamed::new("n", "named"))).expect("write");
    let h = TH1D::new(
        "h",
        "a histogram",
        TAxis::uniform(4, 0.0, 1.0).expect("axis"),
    )
    .with_values_flow(vec![0.0, 1.0, 2.0, 3.0, 4.0, 0.0])
    .expect("values");
    rhizoid_io::update(&path, |f| f.put("h", &h)).expect("update");
    let file = File::open(&path).expect("open");
    let classes: Vec<String> = file
        .streamer_infos()
        .expect("infos")
        .iter()
        .map(|i| i.class_name().to_owned())
        .collect();
    for c in ["TNamed", "TH1D", "TH1", "TAxis"] {
        assert!(
            classes.iter().any(|x| x == c),
            "{c} missing from {classes:?}"
        );
    }
    let back: TH1D = file.get("h").expect("h");
    assert_eq!(back.values_flow(), h.values_flow());
}

#[derive(Row, Debug, PartialEq, Clone)]
struct X {
    x: i32,
}

#[derive(Row)]
struct Y {
    y: f32,
}

#[test]
fn trees_are_extended_as_a_new_cycle() {
    let path = out("extend.root");
    rhizoid_io::create(&path, |f| {
        let mut t = f.tree::<X>("t")?;
        t.extend(&(0..20).map(|x| X { x }).collect::<Vec<_>>())?;
        t.finish()?;
        let mut d = f.mkdir("d")?.tree::<X>("t")?;
        d.fill(&X { x: -1 })?;
        d.finish()
    })
    .expect("write");
    for (round, more) in [(1, 20..35), (2, 35..40)] {
        rhizoid_io::update(&path, |f| {
            let mut t = f.tree::<X>("t")?;
            for x in more.clone() {
                t.fill(&X { x })?;
            }
            t.finish()?;
            let mut d = f.mkdir("d")?.tree::<X>("t")?;
            d.fill(&X { x: -1 - round })?;
            d.finish()
        })
        .expect("update");
        let file = File::open(&path).expect("open");
        let cycles: Vec<u16> = file
            .keys()
            .filter(|k| k.name() == "t")
            .map(|k| k.cycle())
            .collect();
        assert_eq!(
            cycles,
            [u16::try_from(round + 1).expect("cycle")],
            "the old cycle is replaced"
        );
        let t = file.tree("t").expect("tree");
        let x: Vec<i32> = t.branch::<i32>("x").expect("x").read(..).expect("read");
        assert_eq!(x, (0..more.end).collect::<Vec<_>>());
        // The old entries are one cluster, each update's one more.
        let ends: Vec<u64> = t.clusters().iter().map(|c| c.end).collect();
        assert_eq!(
            ends,
            if round == 1 {
                vec![20, 35]
            } else {
                vec![20, 35, 40]
            }
        );
        let d = file.tree("d/t").expect("tree");
        let x: Vec<i32> = d.branch::<i32>("x").expect("x").read(..).expect("read");
        assert_eq!(x, (0..=round).map(|r| -1 - r).collect::<Vec<_>>());
    }
    // Other rows do not fit.
    let e = rhizoid_io::update(&path, |f| f.tree::<Y>("t").map(drop)).expect_err("other branches");
    assert_eq!(e.kind(), ErrorKind::InvalidInput);
}

#[test]
fn rntuples_can_be_added_not_extended() {
    let path = out("ntuples.root");
    rhizoid_io::create(&path, |f| {
        let mut w = f.rntuple::<X>("nt")?;
        w.fill(&X { x: 1 })?;
        w.finish()
    })
    .expect("write");
    rhizoid_io::update(&path, |f| {
        let e = f.rntuple::<X>("nt").expect_err("existing");
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        let mut w = f.mkdir("sub")?.rntuple::<X>("nt2")?;
        w.fill(&X { x: 2 })?;
        w.finish()
    })
    .expect("update");
    let file = File::open(&path).expect("open");
    let rows: Vec<X> = file
        .rntuple("sub/nt2")
        .expect("rntuple")
        .rows::<X>()
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, [X { x: 2 }]);
    assert_eq!(file.rntuple("nt").expect("rntuple").num_entries(), 1);
}

#[test]
fn freed_space_is_listed_and_marked() {
    let path = copy_of("root_dirs.root", "freed.root");
    update_dirs(&path);
    let file = File::open(&path).expect("open");
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{report:?}");
    // The old root key list, a/b's key and key list, a's, the old
    // StreamerInfo and the old free list are gaps now.
    assert!(report.gaps >= 3, "{report:?}");
    assert!(report.free_segments >= 2, "{report:?}");
}

#[test]
fn updates_lock_the_file() {
    let path = copy_of("root_dirs.root", "locked.root");
    let first = WritableFile::update(&path).expect("first");
    let e = WritableFile::update(&path).expect_err("second");
    assert!(e.to_string().contains("locked"), "{e}");
    drop(first);
    WritableFile::update(&path)
        .expect("free again")
        .close()
        .expect("close");
}

#[test]
fn copy_on_write_replaces_the_file() {
    let path = copy_of("root_dirs.root", "cow.root");
    rhizoid_io::update_with(&path, UpdateOptions::default().copy_on_write(true), |f| {
        f.put("cow", &TNamed::new("cow", "copied"))
    })
    .expect("update");
    assert!(dump(&path).contains("NAMED cow copied"));
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("list")
        .filter_map(Result::ok)
        // This file's copy only: another test may be updating its own.
        .filter(|e| e.file_name().to_string_lossy() == ".cow.root.rhizoid-update")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A long file name pushes the root directory record across a 512-byte
/// sector: it cannot be committed in place, so only `copy_on_write` may
/// update the file.
#[test]
fn records_across_sectors_need_copy_on_write() {
    let path = out(&format!("{}.root", "l".repeat(170)));
    rhizoid_io::create(&path, |f| f.put("n", &TNamed::new("n", "top"))).expect("write");
    let file = File::open(&path).expect("open");
    let at = 100 + u64::from(file.header().nbytes_name());
    assert_ne!(
        at / 512,
        (at + 59) / 512,
        "the record should cross a sector"
    );
    drop(file);
    let e = rhizoid_io::update(&path, |f| f.put("m", &TNamed::new("m", "x")))
        .expect_err("not atomic in place");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    assert!(e.to_string().contains("copy_on_write"), "{e}");
    rhizoid_io::update_with(&path, UpdateOptions::default().copy_on_write(true), |f| {
        f.put("m", &TNamed::new("m", "x"))
    })
    .expect("copy on write");
    assert!(dump(&path).contains("NAMED m x"));
}

/// ROOT's dump of a file (`root_dirs.C`), checking it said nothing.
fn root(root: &str, file: &Path, action: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/root_dirs.C");
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l"])
        .arg(format!(
            "{}(\"{}\", \"{action}\")",
            script.display(),
            file.display()
        ))
        .output()
        .expect("run root");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "ROOT failed on {}: {stderr}",
        file.display()
    );
    for line in stdout.lines().chain(stderr.lines()) {
        assert!(
            !(line.contains("Error in <") || line.contains("Warning in <")),
            "ROOT complained on {}: {line}",
            file.display()
        );
    }
    if action == "dump" {
        assert!(stdout.contains("FILE recovered 0"), "{stdout}");
    }
    let mut kept = String::new();
    for l in stdout.lines().filter(|l| !l.starts_with("Processing")) {
        kept.push_str(l);
        kept.push('\n');
    }
    kept
}

#[test]
fn root_reads_our_updates_and_updates_them_itself() {
    let Some(root_exe) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    for fixture in ["root_dirs.root", "ours"] {
        let path = if fixture == "ours" {
            let p = out("root_ours.root");
            rhizoid_io::create(&p, |f| {
                f.put("n", &TNamed::new("n", "top"))?;
                let a = f.mkdir("a")?;
                a.put("n", &TNamed::new("n", "in a"))?;
                a.mkdir("b")?.put("n", &TNamed::new("n", "in a/b"))?;
                f.mkdir("c")?;
                Ok::<_, rhizoid_io::Error>(())
            })
            .expect("write");
            p
        } else {
            copy_of(fixture, "root_theirs.root")
        };
        let before = root(&root_exe, &path, "dump");
        update_dirs(&path);
        let after = root(&root_exe, &path, "dump");
        for line in before.lines() {
            assert!(after.contains(line), "{fixture}: ROOT lost {line}\n{after}");
        }
        for line in [
            "NAMED a/b/u;1 updated a/b",
            "NAMED e/f/u;1 new e/f",
            "NAMED n;2 top, cycle 2",
        ] {
            assert!(
                after.contains(line),
                "{fixture}: ROOT misses {line}\n{after}"
            );
        }
        // Only `a` moves: `a/b` keeps a parent pointer (fSeekParent) to
        // where `a` was, which ROOT must not need.
        rhizoid_io::update(&path, |f| {
            f.mkdir("a")?.put("only_a", &TNamed::new("only_a", "a"))
        })
        .expect("update a");
        assert!(root(&root_exe, &path, "dump").contains("NAMED a/only_a;1 a"));
        // ROOT updates what we updated (writing into a/b); then both read
        // everything.
        root(&root_exe, &path, "update");
        let again = root(&root_exe, &path, "dump");
        for line in after
            .lines()
            .filter(|l| !l.starts_with("KEY a/b;") && !l.starts_with("KEY d"))
        {
            assert!(
                again.contains(line),
                "{fixture}: lost after ROOT's update: {line}\n{again}"
            );
        }
        assert!(
            again.contains("NAMED a/b/m;1 added by ROOT in a/b"),
            "{again}"
        );
        assert!(again.contains("NAMED d/m;1 added by ROOT in d"), "{again}");
        let ours = dump(&path);
        assert!(ours.contains("NAMED a/b/m added by ROOT in a/b"), "{ours}");
        assert!(ours.contains(ADDED), "{ours}");
        // And we update ROOT's update.
        rhizoid_io::update(&path, |f| {
            f.mkdir("a/b")?.put("w", &TNamed::new("w", "third"))
        })
        .expect("third update");
        let last = root(&root_exe, &path, "dump");
        assert!(last.contains("NAMED a/b/w;1 third"), "{last}");
        assert!(
            last.contains("NAMED a/b/m;1 added by ROOT in a/b"),
            "{last}"
        );
    }
}

#[test]
fn root_reads_extended_trees_and_new_classes() {
    let Some(root_exe) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let path = out("root_extend.root");
    rhizoid_io::create(&path, |f| {
        let mut t = f.tree::<X>("t")?;
        t.extend(&(0..20).map(|x| X { x }).collect::<Vec<_>>())?;
        t.finish()
    })
    .expect("write");
    let h = TH1D::new(
        "h",
        "a histogram",
        TAxis::uniform(4, 0.0, 1.0).expect("axis"),
    );
    rhizoid_io::update(&path, |f| {
        f.put("h", &h)?;
        let mut t = f.tree::<X>("t")?;
        for x in 20..35 {
            t.fill(&X { x })?;
        }
        t.finish()
    })
    .expect("update");
    let script = out("root_extend.C");
    std::fs::write(
        &script,
        r#"void root_extend(const char *p) {
  auto f = TFile::Open(p);
  auto t = f->Get<TTree>("t");
  int x = 0, sum = 0;
  t->SetBranchAddress("x", &x);
  for (Long64_t i = 0; i < t->GetEntries(); ++i) { t->GetEntry(i); sum += x; }
  auto it = t->GetClusterIterator(0);
  printf("ENTRIES %lld SUM %d CLUSTERS", t->GetEntries(), sum);
  for (Long64_t s; (s = it()) < t->GetEntries();) printf(" %lld", it.GetNextEntry());
  auto h = f->Get<TH1D>("h");
  printf(" H %d\n", h->GetNbinsX());
}
"#,
    )
    .expect("macro");
    let out = std::process::Command::new(&root_exe)
        .args(["-b", "-q", "-l"])
        .arg(format!("{}(\"{}\")", script.display(), path.display()))
        .output()
        .expect("run root");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        !stderr.contains("Error") && !stderr.contains("Warning"),
        "{stderr}"
    );
    assert!(
        stdout.contains("ENTRIES 35 SUM 595 CLUSTERS 20 35 H 4"),
        "{stdout}{stderr}"
    );
}
