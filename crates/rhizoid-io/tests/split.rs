//! User classes in trees, split as ROOT splits them, round-tripping
//! between ROOT and us. `tests/cross/user_classes.C` (`user_tree_write`)
//! makes ROOT write a tree of a leaf-list branch (`run`), a `UserEvent`
//! branch (`event.`), a `Hit` branch (`hit.`) and a `std::vector<Hit>`
//! branch (`hits`), the fixture `root_user_tree.root`.
//! We read it into the Rust twins of the classes; we write the same rows,
//! the tree object equal to ROOT's; ROOT (with `RHIZOID_ROOT`) reads ours,
//! with its dictionary, as it reads its own; uproot (with
//! `RHIZOID_ORACLE_PYTHON`) reads both the same.

#![cfg(feature = "derive")]

use std::path::{Path, PathBuf};

use rhizoid_io::objects::{Named, Object};
use rhizoid_io::{ErrorKind, File, RootObject, Row};

mod support;

/// `class Hit : public TObject { Int_t fId; Float_t fE[3]; ClassDef(Hit, 1) }`
#[derive(RootObject, Clone, Debug, PartialEq)]
#[root(class = "Hit", version = 1, rename_all = "root")]
struct Hit {
    #[root(base)]
    object: Object,
    id: i32,
    e: [f32; 3],
}

/// The twin of `UserEvent` (version 3).
#[derive(RootObject, Clone, Debug, PartialEq)]
#[root(class = "UserEvent", version = 3, rename_all = "root")]
struct UserEvent {
    #[root(base)]
    named: Named,
    run: i32,
    event: i64,
    p: [f64; 4],
    good: bool,
    flags: u16,
    code: i8,
    mask: u64,
    n: i32,
    #[root(count = n)]
    pt: Vec<f32>,
    tag: String,
    #[root(std_string)]
    label: String,
    ids: Vec<i32>,
    weights: Vec<f64>,
    names: Vec<String>,
    hit: Hit,
}

#[derive(Row, Clone, Debug, PartialEq)]
struct Rec {
    run: i32,
    #[row(rename = "event.")]
    event: UserEvent,
    // Named `hit.` when written.
    hit: Hit,
    hits: Vec<Hit>,
}

/// Entry `i` of `user_tree_write`.
fn rec(i: u8) -> Rec {
    let k = f64::from(i);
    let n = i % 3;
    Rec {
        run: 7 * i32::from(i),
        event: UserEvent {
            named: Named::new(format!("ev{i}"), "a user event"),
            run: 100 + i32::from(i),
            event: 1_234_567_890_123 * (i64::from(i) + 1),
            p: [k, 0.5 + k, 1.0 + k, 1.5 + k],
            good: i.is_multiple_of(2),
            flags: 0xBEEF + u16::from(i),
            code: i8::try_from(b'a' + i).expect("ascii"),
            mask: 0xFF_FFFF_FFFF << i,
            n: i32::from(n),
            pt: (0..n).map(|j| 10.25 * (f32::from(j) + 1.0)).collect(),
            tag: format!("tag {i}"),
            label: format!("label {i}"),
            ids: (0..i % 4).map(|j| 7 * i32::from(j)).collect(),
            weights: (0..i % 4).map(|j| 0.125 * f64::from(j)).collect(),
            names: (0..i % 4).map(|j| format!("n{j}")).collect(),
            hit: Hit {
                object: Object {
                    unique_id: 9,
                    ..Object::default()
                },
                id: 42 + i32::from(i),
                e: [1.5, 2.5 + f32::from(i), -3.0],
            },
        },
        hit: Hit {
            object: Object {
                unique_id: u32::from(i),
                ..Object::default()
            },
            id: 1000 + i32::from(i),
            e: [f32::from(i), 2.0 * f32::from(i), 3.0 * f32::from(i)],
        },
        hits: (0..i)
            .map(|j| Hit {
                object: Object::default(),
                id: 10 * i32::from(i) + i32::from(j),
                // `-j` of an int in C++: 0.0, not -0.0.
                e: [f32::from(j), 0.5 * f32::from(j), f32::from(-i16::from(j))],
            })
            .collect(),
    }
}

fn rows() -> Vec<Rec> {
    (0..5).map(rec).collect()
}

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn out(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rhizoid-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join(name)
}

fn write(path: &Path, rows: &[Rec]) {
    write_with(path, rows, rhizoid_io::tree::TreeOptions::default());
}

fn write_with(path: &Path, rows: &[Rec], options: rhizoid_io::tree::TreeOptions) {
    rhizoid_io::create(path, |f| {
        let mut t = f.tree_with::<Rec>("t", options.title("user classes"))?;
        t.extend(rows)?;
        t.finish()
    })
    .expect("write");
}

fn read(path: &Path) -> Vec<Rec> {
    File::open(path)
        .expect("open")
        .tree("t")
        .expect("tree")
        .rows::<Rec>()
        .expect("rows")
        .map(Result::unwrap)
        .collect()
}

#[test]
fn roots_split_tree_reads_into_the_classes() {
    assert_eq!(read(&data("root_user_tree.root")), rows());
}

#[test]
fn our_split_tree_reads_back() {
    let path = out("ours.root");
    write(&path, &rows());
    let file = File::open(&path).expect("open");
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(read(&path), rows());
    // In clusters of 2: a basket per branch per cluster.
    let clustered = out("clustered.root");
    write_with(
        &clustered,
        &rows(),
        rhizoid_io::tree::TreeOptions::default().cluster_entries(2),
    );
    assert_eq!(read(&clustered), rows());
    // Each member is a branch of its own, read as ROOT's are.
    let tree = file.tree("t").expect("tree");
    let run: Vec<i32> = tree
        .branch::<i32>("event.fRun")
        .expect("fRun")
        .read(..)
        .expect("read");
    assert_eq!(run, [100, 101, 102, 103, 104]);
    let classes: Vec<String> = file
        .streamer_infos()
        .expect("infos")
        .iter()
        .map(|i| i.class_name().to_owned())
        .collect();
    for c in [
        "UserEvent",
        "Hit",
        "TNamed",
        "TBranchElement",
        "TLeafElement",
    ] {
        assert!(
            classes.iter().any(|x| x == c),
            "{c} missing from {classes:?}"
        );
    }
}

/// The members of an object as `path = value` lines, but for `skip`.
fn flatten(prefix: &str, o: &rhizoid_io::DynObject, skip: &[&str], out: &mut Vec<String>) {
    for base in o.bases() {
        flatten(&format!("{prefix}[{}]", base.class()), base, skip, out);
    }
    for (name, v) in o.members() {
        if skip.contains(&name) {
            continue;
        }
        let path = format!("{prefix}.{name}");
        match v {
            rhizoid_io::Value::Object(o) => {
                out.push(format!("{path}: {}", o.class()));
                flatten(&path, o, skip, out);
            }
            rhizoid_io::Value::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    match item {
                        rhizoid_io::Value::Object(o) => {
                            out.push(format!("{path}[{i}]: {}", o.class()));
                            flatten(&format!("{path}[{i}]"), o, skip, out);
                        }
                        v => out.push(format!("{path}[{i}] = {v}")),
                    }
                }
            }
            v => out.push(format!("{path} = {v}")),
        }
    }
}

/// The tree object, branch by branch and leaf by leaf, is ROOT's, but for
/// where the baskets are, how large they came out, and ROOT's empty slots
/// in `fBaskets`.
#[test]
fn split_tree_object_matches_roots() {
    let path = out("object.root");
    write(&path, &rows());
    let dump = |p: &Path| -> Vec<String> {
        let f = File::open(p).expect("open");
        let t: rhizoid_io::DynObject = f.get("t").expect("tree object");
        let mut out = Vec::new();
        let skip = [
            "fTotBytes",
            "fZipBytes",
            "fBasketBytes",
            "fBasketSeek",
            "fBaskets",
        ];
        flatten("t", &t, &skip, &mut out);
        out
    };
    let (ours, theirs) = (dump(&path), dump(&data("root_user_tree.root")));
    for (a, b) in ours.iter().zip(&theirs) {
        assert_eq!(a, b);
    }
    assert_eq!(ours.len(), theirs.len());
}

#[derive(Row)]
struct Events {
    events: Vec<UserEvent>,
}

#[derive(Row)]
struct One {
    event: UserEvent,
}

#[test]
fn what_cannot_be_split_yet_is_refused() {
    let path = out("refused.root");
    rhizoid_io::create(&path, |f| {
        // Strings and vectors inside a collection's elements.
        let e = f.tree::<Events>("t").expect_err("not yet");
        assert_eq!(e.kind(), ErrorKind::Unsupported, "{e}");
        let e = f.rntuple::<One>("n").expect_err("not yet");
        assert_eq!(e.kind(), ErrorKind::Unsupported);
        Ok::<_, rhizoid_io::Error>(())
    })
    .expect("write");
}

fn root_user(root: &str, action: &str, file: &Path) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/user_classes.C");
    // Per process: other test runs may be compiling the same macro.
    let build = std::env::temp_dir().join(format!("rhizoid-aclic-user-{}", std::process::id()));
    std::fs::create_dir_all(&build).expect("build dir");
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l", "-e"])
        .arg(format!(
            "gSystem->SetBuildDir(\"{}\", true);",
            build.display()
        ))
        .arg("-e")
        .arg(format!(".L {}+", script.display()))
        .arg("-e")
        .arg(format!("user_{action}(\"{}\");", file.display()))
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
            "ROOT complained reading {}: {line}",
            file.display()
        );
    }
    assert!(stdout.contains("FILE recovered 0"), "{stdout}");
    let mut kept = String::new();
    for l in stdout
        .lines()
        .filter(|l| !l.starts_with("Processing") && !l.starts_with("Info in <TUnixSystem::ACLiC>"))
    {
        kept.push_str(l);
        kept.push('\n');
    }
    kept
}

#[test]
fn root_reads_our_split_tree_as_its_own() {
    let Some(root) = std::env::var("RHIZOID_ROOT").ok() else {
        return;
    };
    let path = out("root.root");
    write(&path, &rows());
    let theirs = root_user(&root, "tree_dump", &data("root_user_tree.root"));
    assert!(theirs.contains("hits/4/3 id 43"), "{theirs}");
    assert_eq!(root_user(&root, "tree_dump", &path), theirs);
    // Several clusters: several baskets per branch.
    let clustered = out("root_clustered.root");
    write_with(
        &clustered,
        &rows(),
        rhizoid_io::tree::TreeOptions::default().cluster_entries(2),
    );
    assert_eq!(root_user(&root, "tree_dump", &clustered), theirs);
}

fn uproot(python: &str, file: &Path) -> String {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_dump_tree.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .arg("t")
        .arg("leaves")
        .output()
        .expect("run python");
    assert!(
        out.status.success(),
        "uproot failed on {}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn uproot_reads_our_split_tree_as_roots() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    let path = out("uproot.root");
    write(&path, &rows());
    // uproot reads split objects branch by branch (it cannot group nested
    // bases, even in ROOT's own file): every branch holding data.
    let theirs = uproot(&python, &data("root_user_tree.root"));
    assert!(
        theirs.contains("event.fHit.fId values 42 43 44 45 46"),
        "{theirs}"
    );
    assert_eq!(uproot(&python, &path), theirs);
}

/// Each branch's byte totals, the split ones' and their parents', are
/// their baskets'; the tree's are their sum.
#[test]
fn split_byte_totals_are_the_baskets() {
    let path = out("totals.root");
    // A basket an entry: more than the 10 slots ROOT starts with.
    let rows: Vec<Rec> = (0..12).map(|i| rec(i % 5)).collect();
    write_with(
        &path,
        &rows,
        rhizoid_io::tree::TreeOptions::default().cluster_entries(1),
    );
    let baskets = support::accounting::check_byte_totals(&path, "t");
    assert!(baskets > 12 * 20, "{baskets} baskets");
    assert_eq!(read(&path), rows);
}

/// Split branches count toward the byte budget: clusters end before the
/// last entry, all of one size.
#[test]
fn split_branches_count_toward_the_byte_budget() {
    let path = out("budget.root");
    let rows: Vec<Rec> = (0..60).map(|i| rec(i % 7)).collect();
    write_with(
        &path,
        &rows,
        rhizoid_io::tree::TreeOptions::default().cluster_bytes(4000),
    );
    let file = File::open(&path).expect("open");
    let clusters = file.tree("t").expect("tree").clusters().to_vec();
    assert!(clusters.len() > 2, "{clusters:?}");
    let first = clusters[0].end;
    assert!(
        clusters[..clusters.len() - 1]
            .iter()
            .all(|c| c.end - c.start == first),
        "{clusters:?}"
    );
    assert_eq!(read(&path), rows);
}
