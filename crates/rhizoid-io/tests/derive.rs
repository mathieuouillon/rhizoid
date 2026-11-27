//! User classes with `#[derive(RootObject)]`, against twins compiled
//! by ROOT (`tests/cross/user_classes.C`): the same description, checksum
//! included; ROOT's objects read; ours read back, and (with `RHIZOID_ROOT`)
//! read by ROOT with its dictionary, without a warning; schema evolution
//! between versions. Typed rows with `#[derive(Row)]` and tuples.

#![cfg(feature = "derive")]

use std::path::{Path, PathBuf};

use rhizoid_io::objects::{TNamed, TObject};
use rhizoid_io::{File, RootObject, Row};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// `class Hit : public TObject { Int_t fId; Float_t fE[3]; ClassDef(Hit, 1) }`
#[derive(RootObject, Clone, Debug, PartialEq)]
#[root(class = "Hit", version = 1, rename_all = "root")]
struct Hit {
    #[root(base)]
    object: TObject,
    id: i32,
    e: [f32; 3],
}

/// The twin of `UserEvent` (version 3).
#[derive(RootObject, Clone, Debug, PartialEq)]
#[root(class = "UserEvent", version = 3, rename_all = "root")]
struct UserEvent {
    #[root(base)]
    named: TNamed,
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

fn event(i: u8) -> UserEvent {
    let k = f64::from(i);
    UserEvent {
        named: TNamed::new(format!("ev{i}"), "a user event"),
        run: 100 + i32::from(i),
        event: 1_234_567_890_123 * (i64::from(i) + 1),
        p: [k, 0.5 + k, 1.0 + k, 1.5 + k],
        good: i.is_multiple_of(2),
        flags: 0xBEEF + u16::from(i),
        code: i8::try_from(b'a' + i).expect("ascii"),
        mask: 0xFF_FFFF_FFFF << i,
        n: i32::from(i) + 1,
        pt: (0..=i).map(|j| 10.25 * (f32::from(j) + 1.0)).collect(),
        tag: format!("tag {i}"),
        label: format!("label {i}"),
        ids: (0..=i).map(|j| 7 * i32::from(j)).collect(),
        weights: (0..=i).map(|j| 0.125 * f64::from(j)).collect(),
        names: (0..=i).map(|j| format!("n{j}")).collect(),
        hit: Hit {
            object: TObject {
                unique_id: 9,
                ..TObject::default()
            },
            id: 42 + i32::from(i),
            e: [1.5, 2.5 + f32::from(i), -3.0],
        },
    }
}

/// Our description of a class equals the one ROOT wrote from its
/// dictionary: every element, and the checksum ROOT checks.
#[test]
fn descriptions_are_root_s() {
    let file = File::open(data("root_user.root")).expect("open");
    let theirs = file.streamer_infos().expect("descriptions");
    for ours in [UserEvent::streamer_info(), Hit::streamer_info()] {
        let root = theirs
            .iter()
            .find(|i| i.class_name() == ours.class_name())
            .expect("ROOT's description");
        assert_eq!(
            format!("{:#010x}", ours.checksum()),
            format!("{:#010x}", root.checksum()),
            "{} checksum",
            ours.class_name()
        );
        assert_eq!(ours.class_version(), root.class_version());
        assert_eq!(ours.elements().len(), root.elements().len());
        for (a, b) in ours.elements().iter().zip(root.elements()) {
            assert_eq!(
                (a.name(), a.type_name(), a.type_code(), a.kind()),
                (b.name(), b.type_name(), b.type_code(), b.kind()),
                "{}",
                ours.class_name()
            );
        }
    }
}

/// ROOT's objects read into the twins, as ROOT read them.
#[test]
fn we_read_root_s_user_objects() {
    let file = File::open(data("root_user.root")).expect("open");
    for i in 0..3_u8 {
        let ev: UserEvent = file.get(&format!("ev{i}")).expect("read");
        let mut expected = event(i);
        // ROOT writes TObject's bits as it sets them.
        expected.named = ev.named.clone();
        expected.hit.object = ev.hit.object;
        assert_eq!(ev, expected, "ev{i}");
        assert_eq!(ev.named.name(), format!("ev{i}"));
        assert_eq!(ev.hit.object.unique_id, 9);
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rhizoid-derive-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("tmp");
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_ours(path: &Path) {
    rhizoid_io::create(path, |f| {
        for i in 0..3 {
            f.put(&format!("ev{i}"), &event(i))?;
        }
        Ok::<(), rhizoid_io::Error>(())
    })
    .expect("write");
}

/// Ours read back, with the descriptions stored.
#[test]
fn our_user_objects_read_back() {
    let dir = TempDir::new("back");
    let path = dir.0.join("ours.root");
    write_ours(&path);
    let file = File::open(&path).expect("open");
    for i in 0..3 {
        let ev: UserEvent = file.get(&format!("ev{i}")).expect("read");
        assert_eq!(ev, event(i));
    }
    let classes: Vec<String> = file
        .streamer_infos()
        .expect("descriptions")
        .iter()
        .map(|i| i.class_name().to_owned())
        .collect();
    for c in ["UserEvent", "Hit", "TNamed", "TObject"] {
        assert!(
            classes.iter().any(|x| x == c),
            "{c} not stored: {classes:?}"
        );
    }
    assert!(file.check().expect("check").is_ok());
}

/// ROOT, with the classes' dictionary, reads ours as we wrote them, and
/// does not warn (the checksums match).
#[test]
fn root_reads_our_user_objects() {
    if std::env::var_os("RHIZOID_ROOT").is_none() {
        return;
    }
    let dir = TempDir::new("root");
    let path = dir.0.join("ours.root");
    write_ours(&path);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("bash")
        .arg(repo.join("tools/root-user.sh"))
        .arg("dump")
        .arg(&path)
        .current_dir(&repo)
        .output()
        .expect("run root");
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        assert!(
            !(line.contains("Error in <") || line.contains("Warning in <")),
            "ROOT complained: {line}"
        );
    }
    let expected = std::fs::read_to_string(data("root_user.txt")).expect("ROOT's own reading");
    assert_eq!(
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>(),
        expected
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>(),
        "ROOT reads ours as it reads its own"
    );
}

/// Version 4 of the class, as a later program would declare it: a member
/// renamed (read through its old name), one widened, one removed, one
/// added. It reads version 3 objects, ROOT's and ours.
#[derive(RootObject, Debug, PartialEq)]
#[root(class = "UserEvent", version = 4, rename_all = "root")]
struct UserEventV4 {
    #[root(base)]
    named: TNamed,
    #[root(rename = "fRunNumber", alias = "fRun")]
    run_number: i64,
    p: [f64; 4],
    #[root(std_string)]
    label: String,
    weights: Vec<f32>,
    added: f64,
    #[root(skip)]
    cache: Option<u8>,
}

#[test]
fn newer_versions_read_older_objects() {
    let file = File::open(data("root_user.root")).expect("open");
    let v4: UserEventV4 = file.get("ev2").expect("evolve");
    assert_eq!(v4.run_number, 102);
    assert_eq!(v4.p, [2.0, 2.5, 3.0, 3.5]);
    assert_eq!(v4.label, "label 2");
    assert_eq!(v4.weights, [0.0, 0.125, 0.25]);
    assert_eq!((v4.added, v4.cache), (0.0, None));

    // And back: v3 reads what v4 wrote (fRunNumber is not fRun: 0).
    let dir = TempDir::new("evolve");
    let path = dir.0.join("v4.root");
    let v4 = UserEventV4 {
        named: TNamed::new("e", "t"),
        run_number: 7,
        p: [1.0; 4],
        label: "x".into(),
        weights: vec![0.5],
        added: 2.0,
        cache: Some(1),
    };
    rhizoid_io::create(&path, |f| f.put("e", &v4)).expect("write");
    let file = File::open(&path).expect("open");
    let back: UserEventV4 = file.get("e").expect("same version");
    assert_eq!(back, UserEventV4 { cache: None, ..v4 });
    let v3: UserEvent = file.get("e").expect("older version");
    assert_eq!((v3.run, v3.p, v3.label.as_str()), (0, [1.0; 4], "x"));
    assert_eq!(v3.weights, [0.5]);
    assert!(v3.pt.is_empty() && v3.ids.is_empty());
}

#[test]
fn objects_of_another_class_are_refused() {
    let file = File::open(data("root_hists.root")).expect("open");
    let e = file.get::<UserEvent>("named").expect_err("a TNamed");
    assert_eq!(e.kind(), rhizoid_io::ErrorKind::ClassMismatch);
}

/// A typed row of the tree fixture.
#[derive(Row, Debug, PartialEq)]
struct Jets {
    #[row(rename = "njet_pt")]
    n: i32,
    jet_pt: Vec<f32>,
    xyz: [f32; 3],
    b: bool,
    u64: u64,
}

#[test]
fn rows_of_a_tree() {
    let file = File::open(data("uproot_trees.root")).expect("open");
    let tree = file.tree("events").expect("tree");
    let rows: Vec<Jets> = tree
        .rows::<Jets>()
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("read");
    assert_eq!(rows.len(), 1001);
    let jet_pt = tree
        .branch::<rhizoid_io::columnar::Jagged<f32>>("jet_pt")
        .expect("b")
        .read(..)
        .expect("read");
    let xyz = tree
        .branch::<[f32; 3]>("xyz")
        .expect("b")
        .read(..)
        .expect("read");
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.jet_pt, jet_pt.get(i).expect("entry"));
        assert_eq!(usize::try_from(r.n).expect("count"), r.jet_pt.len());
        assert_eq!(r.xyz, xyz[i]);
    }
    // Tuples, with names; a selection through the builder.
    let pairs: Vec<(f64, Vec<i32>)> = tree
        .reader()
        .selection(10..20)
        .expect("selection")
        .rows_of::<(f64, Vec<i32>), 2>(["f64", "jet_q"])
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("read");
    assert_eq!(pairs.len(), 10);
    let f64s = tree
        .branch::<f64>("f64")
        .expect("b")
        .read(10..20)
        .expect("read");
    assert_eq!(pairs.iter().map(|p| p.0).collect::<Vec<_>>(), f64s);
    // A field that cannot read its column is refused once, at open.
    let e = tree
        .rows_of::<(f32,), 1>(["jet_pt"])
        .expect_err("jagged as scalar");
    assert_eq!(e.kind(), rhizoid_io::ErrorKind::TypeMismatch);
    assert!(tree.rows_of::<(f32, f32), 1>(["f32"]).is_err());
}

#[derive(Row, Debug, PartialEq)]
struct Particles {
    x: f64,
    jet_pt: Vec<f32>,
    label: String,
    flag: bool,
}

#[test]
fn rows_of_an_rntuple() {
    let file = File::open(data("uproot_rntuple.root")).expect("open");
    let nt = file.rntuple("events").expect("rntuple");
    let rows: Vec<Particles> = nt
        .rows::<Particles>()
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("read");
    assert_eq!(rows.len(), 500);
    let labels = nt
        .field::<String>("label")
        .expect("f")
        .read(..)
        .expect("read");
    let x = nt.field::<f64>("x").expect("f").read(..).expect("read");
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.label, labels[i]);
        assert_eq!(r.x, x[i]);
    }
}
