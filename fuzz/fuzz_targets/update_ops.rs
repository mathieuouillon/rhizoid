//! Update mode against a model: sequences of updates (objects put in nested
//! directories, trees created and extended), some failing on purpose, with
//! or without copy-on-write. After each, the file must hold exactly what
//! the model says; a failed update must leave it as it was.
#![no_main]

use std::collections::BTreeMap;
use std::path::PathBuf;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rhizoid_io::objects::Named;
use rhizoid_io::{File, FileScope, Row, UpdateOptions};

/// Directories, from the top one: names never collide with keys.
const DIRS: [&str; 4] = ["", "d0", "d1", "d0/d2"];

#[derive(Row, Clone, Debug, PartialEq, Arbitrary)]
struct Entry {
    a: i32,
    b: f32,
}

#[derive(Arbitrary, Debug)]
enum Op {
    Put { dir: u8, name: u8, title: String },
    Tree { dir: u8, name: u8, rows: Vec<Entry> },
}

#[derive(Arbitrary, Debug)]
struct Step {
    ops: Vec<Op>,
    fail: bool,
    copy_on_write: bool,
}

#[derive(Default, Clone, PartialEq, Debug)]
struct Model {
    /// (directory, name) → the latest title.
    objects: BTreeMap<(usize, String), String>,
    /// (directory, name) → every row, old and new.
    trees: BTreeMap<(usize, String), Vec<Entry>>,
}

fn path() -> PathBuf {
    std::env::temp_dir().join(format!("rhizoid-fuzz-update-{}.root", std::process::id()))
}

fn full(dir: usize, name: &str) -> String {
    if DIRS[dir].is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", DIRS[dir])
    }
}

/// Applies a step's operations to the file, and to `model` as it goes.
fn apply(f: &FileScope<'_>, ops: &[Op], model: &mut Model) -> rhizoid_io::Result<()> {
    let mut trees_this_step = Vec::new();
    for op in ops.iter().take(8) {
        match op {
            Op::Put { dir, name, title } => {
                let (dir, name) = (usize::from(*dir) % DIRS.len(), format!("k{}", name % 4));
                let named = Named::new(name.clone(), title.clone());
                if dir == 0 {
                    f.put(&name, &named)?;
                } else {
                    f.mkdir(DIRS[dir])?.put(&name, &named)?;
                }
                model.objects.insert((dir, name), title.clone());
            }
            Op::Tree { dir, name, rows } => {
                let (dir, name) = (usize::from(*dir) % DIRS.len(), format!("t{}", name % 2));
                // One writer per tree in an update.
                if trees_this_step.contains(&(dir, name.clone())) {
                    continue;
                }
                trees_this_step.push((dir, name.clone()));
                let rows: Vec<Entry> = rows.iter().take(16).cloned().collect();
                let mut w = if dir == 0 {
                    f.tree::<Entry>(&name)?
                } else {
                    f.mkdir(DIRS[dir])?.tree::<Entry>(&name)?
                };
                w.extend(&rows)?;
                w.finish()?;
                model.trees.entry((dir, name)).or_default().extend(rows);
            }
        }
    }
    Ok(())
}

/// The file must hold exactly what the model says.
fn check(model: &Model) {
    let file = File::open(path()).expect("open");
    assert!(file.check().expect("check").is_ok());
    for ((dir, name), title) in &model.objects {
        let n: Named = file.get(&full(*dir, name)).expect("an object of the model");
        assert_eq!(n.title(), title, "{}", full(*dir, name));
    }
    for ((dir, name), rows) in &model.trees {
        let t = file.tree(&full(*dir, name)).expect("a tree of the model");
        let back: Vec<Entry> = t
            .rows::<Entry>()
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect();
        let bits = |v: &[Entry]| v.iter().map(|e| (e.a, e.b.to_bits())).collect::<Vec<_>>();
        assert_eq!(bits(&back), bits(rows), "{}", full(*dir, name));
        let cycles = file
            .root()
            .subdir(DIRS[*dir])
            .map(|d| d.keys().filter(|k| k.name() == name).count())
            .unwrap_or_else(|_| file.keys().filter(|k| k.name() == name).count());
        assert_eq!(cycles, 1, "an extended tree replaces its cycle");
    }
    // Nothing else: every key of a directory is in the model.
    for (dir, path) in DIRS.iter().enumerate() {
        let Ok(d) = (if path.is_empty() {
            Ok(file.root())
        } else {
            file.root().subdir(path)
        }) else {
            continue;
        };
        for k in d.keys().filter(|k| !k.is_directory()) {
            let key = (dir, k.name().to_owned());
            assert!(
                model.objects.contains_key(&key) || model.trees.contains_key(&key),
                "{} is in no update",
                full(dir, k.name())
            );
        }
    }
}

fuzz_target!(|steps: Vec<Step>| {
    let path = path();
    rhizoid_io::create(&path, |_| Ok::<(), rhizoid_io::Error>(())).expect("create");
    let mut model = Model::default();
    for step in steps.iter().take(4) {
        let mut next = model.clone();
        let options = UpdateOptions::default().copy_on_write(step.copy_on_write);
        let result = rhizoid_io::update_with(&path, options, |f| {
            apply(f, &step.ops, &mut next)?;
            if step.fail {
                return Err(rhizoid_io::Error::new(
                    rhizoid_io::ErrorKind::Other,
                    "abandoned",
                ));
            }
            Ok(())
        });
        match result {
            Ok(()) => model = next,
            Err(e) => assert!(step.fail, "an update failed: {e}"),
        }
        check(&model);
    }
    let _ = std::fs::remove_file(&path);
});
